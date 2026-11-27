#!/usr/bin/env python3
"""Directed tests of interrupt timing in the model (python3 -m unittest).

Neither Harte's single-step tests nor nestest cover interrupts. These pin
the documented 6502 behaviour (nesdev wiki, "CPU interrupts") until the
suites that do (Klaus Dormann's interrupt test, blargg's
cpu_interrupts_v2) can run.
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from cpu import B, I, Cpu  # noqa: E402

RESET_TO, NMI_TO, IRQ_TO = 0x8000, 0x9000, 0xA000


class Machine:
    """64 KiB of RAM, a program at $8000, vectors set, bus trace kept."""

    def __init__(self, program, irq_at=None, nmi_at=None):
        self.mem = bytearray(0x10000)
        self.mem[RESET_TO:RESET_TO + len(program)] = bytes(program)
        for vec, to in ((0xFFFA, NMI_TO), (0xFFFC, RESET_TO), (0xFFFE, IRQ_TO)):
            self.mem[vec], self.mem[vec + 1] = to & 0xFF, to >> 8
        # NOPs in the handlers, so a handler's first instruction is visible.
        self.mem[NMI_TO:NMI_TO + 8] = b"\xEA" * 8
        self.mem[IRQ_TO:IRQ_TO + 8] = b"\xEA" * 8
        self.writes = []
        self.cpu = Cpu(self.mem.__getitem__, self._write)
        self.cpu.s, self.cpu.p = 0, 0x00
        self.cpu.reset()
        self.cpu.step()
        self.start = self.cpu.cycles  # cycle numbers below count from here
        self.irq_at, self.nmi_at = irq_at, nmi_at
        self.cpu.on_cycle = self._lines

    def _write(self, a, v):
        self.writes.append((a, v))
        self.mem[a] = v

    def _lines(self, cpu):
        n = cpu.cycles - self.start
        cpu.irq = self.irq_at is not None and n >= self.irq_at
        cpu.nmi = self.nmi_at is not None and n >= self.nmi_at

    def run(self, instructions):
        """Step that many times; return the PC at the start of each step."""
        pcs = []
        for _ in range(instructions):
            pcs.append(self.cpu.pc)
            self.cpu.step()
        return pcs


class Reset(unittest.TestCase):
    def test_reset_sequence(self):
        m = Machine([0xEA])
        self.assertEqual(m.cpu.pc, RESET_TO)
        self.assertEqual(m.cpu.s, 0xFD, "three stack reads, S - 3")
        self.assertTrue(m.cpu.p & I)
        self.assertEqual(m.writes, [], "reset writes nothing")
        self.assertEqual(m.start, 7)


class Irq(unittest.TestCase):
    def test_cli_lets_one_instruction_run_first(self):
        # CLI with IRQ already low: the NOP after CLI runs, then the IRQ.
        m = Machine([0x58, 0xEA, 0xEA, 0xEA], irq_at=0)
        pcs = m.run(3)
        self.assertEqual(pcs, [0x8000, 0x8001, 0x8002])
        self.assertEqual(m.cpu.pc, IRQ_TO)
        # Pushed: return address $8002, then P with B clear.
        self.assertEqual(m.writes[:2], [(0x1FD, 0x80), (0x1FC, 0x02)])
        self.assertFalse(m.writes[2][1] & B)

    def test_sei_still_takes_a_pending_irq_with_i_pushed_set(self):
        # Cycles: CLI 0-1, NOP 2-3, SEI 4-5. IRQ from SEI's first cycle,
        # which is its poll point: SEI's I has not taken effect yet.
        m = Machine([0x58, 0xEA, 0x78, 0xEA], irq_at=4)
        m.run(3)  # CLI, NOP, SEI
        m.run(1)  # the IRQ sequence
        self.assertEqual(m.cpu.pc, IRQ_TO)
        self.assertTrue(m.writes[2][1] & I, "P pushed after SEI has I set")

    def test_irq_ignored_while_i_set(self):
        m = Machine([0xEA] * 8, irq_at=0)
        self.assertEqual(m.run(5), [0x8000 + i for i in range(5)])

    def test_interrupt_sequence_takes_seven_cycles(self):
        m = Machine([0x58, 0xEA, 0xEA], irq_at=0)
        m.run(2)
        c = m.cpu.cycles
        m.run(1)
        self.assertEqual(m.cpu.cycles - c, 7)


class Branch(unittest.TestCase):
    # LDA #0 sets Z, then a taken BEQ +0 that does not cross a page.
    PROG = [0x58, 0xA9, 0x00, 0xF0, 0x00, 0xEA, 0xEA, 0xEA]

    def test_taken_branch_without_crossing_delays_irq(self):
        # Cycles from the CLI: CLI 0-1, LDA 2-3, BEQ 4-6. IRQ at cycle 5
        # (the branch's operand cycle, which does not poll): not taken after
        # the branch, but after the next instruction.
        m = Machine(self.PROG, irq_at=5)
        pcs = m.run(4)
        self.assertEqual(pcs, [0x8000, 0x8001, 0x8003, 0x8005])
        m.run(1)
        self.assertEqual(m.cpu.pc, IRQ_TO)

    def test_same_timing_without_a_branch_is_taken_at_once(self):
        # LDA zp (3 cycles) in place of the branch, IRQ in its second cycle.
        m = Machine([0x58, 0xA9, 0x00, 0xA5, 0x00, 0xEA, 0xEA], irq_at=5)
        pcs = m.run(3)
        self.assertEqual(pcs, [0x8000, 0x8001, 0x8003])
        m.run(1)
        self.assertEqual(m.cpu.pc, IRQ_TO)


class Nmi(unittest.TestCase):
    def test_nmi_is_edge_triggered(self):
        m = Machine([0xEA] * 4, nmi_at=0)  # held low from the start
        m.run(1)  # NOP
        m.run(1)  # NMI
        self.assertEqual(m.cpu.pc, NMI_TO)
        self.assertEqual(m.run(4), [NMI_TO + i for i in range(4)], "taken once")

    def test_nmi_hijacks_brk(self):
        # NMI asserted during BRK's first pushes: BRK takes the NMI vector,
        # and the pushed P still has B set.
        m = Machine([0x00, 0x00], nmi_at=2)
        m.run(1)
        self.assertEqual(m.cpu.pc, NMI_TO)
        self.assertTrue(m.writes[2][1] & B)

    def test_nmi_after_vector_choice_waits(self):
        # NMI asserted after BRK chose its vector: BRK goes to the IRQ
        # vector, and the NMI follows.
        m = Machine([0x00, 0x00], nmi_at=5)
        m.run(1)
        self.assertEqual(m.cpu.pc, IRQ_TO)
        self.assertTrue(m.cpu.nmi_pending)


if __name__ == "__main__":
    unittest.main()
