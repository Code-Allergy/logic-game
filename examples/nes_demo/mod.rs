//! A tiny hand-assembled NROM program for the issue #117 experiments: it
//! does what games do every frame (sprites on, 64 sprites from a RAM page
//! copied by OAM DMA in the NMI, scroll writes, a main loop moving the
//! sprites), unlike nestest's idle menu (sprites off). Patched into the
//! console's PRG ROM (`--demo`); CHR stays nestest's.

use logi_sim::compile::Program;

#[rustfmt::skip]
const CODE: &[u8] = &[
    // C000 reset
    0x78, 0xD8, 0xA2, 0xFF, 0x9A,             // SEI CLD LDX #$FF TXS
    0x2C, 0x02, 0x20, 0x10, 0xFB,             // C005 BIT $2002 BPL C005
    0x2C, 0x02, 0x20, 0x10, 0xFB,             // C00A BIT $2002 BPL C00A
    0xA2, 0x00,                               // C00F LDX #0
    0x8A, 0x4A,                               // C011 TXA LSR
    0x9D, 0x00, 0x02, 0x9D, 0x01, 0x02,       // STA $0200,X STA $0201,X
    0x29, 0x03, 0x9D, 0x02, 0x02,             // AND #3 STA $0202,X
    0x8A, 0x9D, 0x03, 0x02,                   // TXA STA $0203,X
    0xE8, 0xE8, 0xE8, 0xE8, 0xD0, 0xE9,       // INX x4 BNE C011
    0xA9, 0x1E, 0x8D, 0x01, 0x20,             // C028 LDA #$1E STA $2001
    0xA9, 0x80, 0x8D, 0x00, 0x20,             // C02D LDA #$80 STA $2000
    0xA2, 0x00,                               // C032 LDX #0
    0xFE, 0x03, 0x02,                         // C034 INC $0203,X
    0xE8, 0xE8, 0xE8, 0xE8, 0xD0, 0xF7,       // INX x4 BNE C034
    0x4C, 0x32, 0xC0,                         // C03D JMP C032
    // C040 NMI
    0x48, 0xA9, 0x00, 0x8D, 0x03, 0x20,       // PHA LDA #0 STA $2003
    0xA9, 0x02, 0x8D, 0x14, 0x40,             // LDA #2 STA $4014
    0x8D, 0x05, 0x20, 0x8D, 0x05, 0x20,       // STA $2005 STA $2005
    0x68, 0x40,                               // C051 PLA C052 RTI
];

/// Patch the program into the 32 KiB PRG ROM of `p` (at $C000, vectors
/// at $FFFA).
pub fn patch(p: &mut Program) {
    let m = p
        .mems
        .iter_mut()
        .find(|m| m.addr_bits == 15 && !m.writable && m.width == 8)
        .expect("a 32 KiB PRG ROM");
    let mut v = m.init.to_vec();
    v.resize(1 << 15, 0);
    for (k, &b) in CODE.iter().enumerate() {
        v[0x4000 + k] = u64::from(b);
    }
    for (k, b) in [0x40u8, 0xC0, 0x00, 0xC0, 0x52, 0xC0].iter().enumerate() {
        v[0x7FFA + k] = u64::from(*b);
    }
    m.init = v.into();
}

/// `p`, patched when the environment variable `NES_DEMO` is set.
pub fn maybe(mut p: Program) -> Program {
    if std::env::var_os("NES_DEMO").is_some() {
        patch(&mut p);
    }
    p
}
