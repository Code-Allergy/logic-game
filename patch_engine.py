import re

with open("logi-sim/src/engine/mod.rs", "r") as f:
    content = f.read()

# Add runner_type to SimulationEngine
content = content.replace(
"""pub struct SimulationEngine {
    pub circuit: Circuit,
    pub shared_state: Arc<SharedState>,
    command_tx: Option<Sender<SimCommand>>,
    /// Internal event receiver when the caller hasn't provided an external Sender.
    event_rx: Option<Receiver<SimEvent>>,
    /// External event sender provided by the caller (e.g. SimBridge).
    external_event_tx: Option<Sender<SimEvent>>,
    worker_handle: Option<JoinHandle<()>>,
}""",
"""pub struct SimulationEngine {
    pub circuit: Circuit,
    pub shared_state: Arc<SharedState>,
    runner_type: RunnerType,
    command_tx: Option<Sender<SimCommand>>,
    /// Internal event receiver when the caller hasn't provided an external Sender.
    event_rx: Option<Receiver<SimEvent>>,
    /// External event sender provided by the caller (e.g. SimBridge).
    external_event_tx: Option<Sender<SimEvent>>,
    worker_handle: Option<JoinHandle<()>>,
}"""
)

# Initialize runner_type in new()
content = content.replace(
"""        Self {
            circuit,
            shared_state: Arc::new(SharedState::new()),
            command_tx: None,""",
"""        Self {
            circuit,
            shared_state: Arc::new(SharedState::new()),
            runner_type: RunnerType::default(),
            command_tx: None,"""
)

# Update new_with_event_channel
content = content.replace(
"""    pub fn new_with_event_channel(circuit: Circuit, event_tx: Sender<SimEvent>) -> Self {
        Self {
            circuit,
            shared_state: Arc::new(SharedState::new()),
            command_tx: None,""",
"""    pub fn new_with_event_channel(circuit: Circuit, event_tx: Sender<SimEvent>) -> Self {
        Self {
            circuit,
            shared_state: Arc::new(SharedState::new()),
            runner_type: RunnerType::default(),
            command_tx: None,"""
)

with open("logi-sim/src/engine/mod.rs", "w") as f:
    f.write(content)
