import re

with open("logi-sim/src/engine/mod.rs", "r") as f:
    content = f.read()

content = content.replace("impl Runner for MappedRunner {", "impl crate::runner::Runner for MappedRunner {")

with open("logi-sim/src/engine/mod.rs", "w") as f:
    f.write(content)
