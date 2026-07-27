with open("logi-sim/src/engine/mod.rs", "r") as f:
    content = f.read()

# Make sure Step does `if needs_rebuild { runner = build_runner... }`
import re

content = content.replace("""                        SimCommand::Step => {
                            runner.tick();""", """                        SimCommand::Step => {
                            if needs_rebuild {
                                runner = build_runner(&circuit, current_runner_type);
                                needs_rebuild = false;
                            }
                            runner.tick();""")

with open("logi-sim/src/engine/mod.rs", "w") as f:
    f.write(content)
