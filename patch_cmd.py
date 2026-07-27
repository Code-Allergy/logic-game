with open("logi-sim/src/engine/mod.rs", "r") as f:
    content = f.read()

content = content.replace("SimCommand::SetTickRate(rate) => {", "SimCommand::SetRunner(runner_type) => {\n                            // Handled later\n                        }\n                        SimCommand::SetTickRate(rate) => {")

with open("logi-sim/src/engine/mod.rs", "w") as f:
    f.write(content)
