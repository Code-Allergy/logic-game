#!/bin/sh
# Generate every request file of the `nes` workspace into nes/build, in
# the order they are applied (docs: nes/gen/README.md):
#   1 cells.json  2 probes.json  3 import_rtl cpu6502_ref.v  4 alu.json
#   5 cpu.json  6 tb.json
set -e
cd "$(dirname "$0")"
mkdir -p ../build
export PYTHONDONTWRITEBYTECODE=1
python3 cells.py > ../build/cells.json
python3 probes.py > ../build/probes.json
python3 verilog.py > ../build/cpu6502_ref.v
python3 alu.py > ../build/alu.json
python3 cpu.py > ../build/cpu.json
python3 tb.py --gates > ../build/tb.json
python3 video.py > ../build/video.json
python3 console.py > ../build/console.json
echo ok
