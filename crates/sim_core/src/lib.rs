use ahash::AHashMap;
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use uuid::Uuid;

#[cfg(test)]
mod tests;

pub mod circuits;
pub mod gates;

pub type ComponentId = Uuid;
pub type GateId = ComponentId;
pub type CircuitId = ComponentId;
pub type PinId = usize;

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum TickError {
    CycleDetected,
    InputGateMismatch,
    OutputGateMismatch,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LogicComponent {
    Gate(Gate),
    Circuit(Circuit),
}

// Generic wrapper for a logic component
impl LogicComponent {
    pub fn id(&self) -> ComponentId {
        match self {
            LogicComponent::Gate(gate) => gate.id.gate_id,
            LogicComponent::Circuit(circuit) => circuit.id,
        }
    }

    pub fn get_type(&self) -> GateType {
        match self {
            LogicComponent::Gate(gate) => gate.gate_type,
            LogicComponent::Circuit(_) => GateType::Circuit,
        }
    }

    pub fn set_input(&mut self, pin: PinId, value: Option<bool>) {
        match self {
            LogicComponent::Gate(gate) => {
                if pin < gate.inputs.len() {
                    gate.inputs[pin] = value;
                }
            }
            LogicComponent::Circuit(circuit) => {
                if let Some(gate_id) = circuit.input_gates.get(&pin) {
                    if let Some(gate) = circuit.components.get_mut(gate_id) {
                        gate.set_input(0, value);
                    }
                }
            }
        }
    }

    pub fn get_input(&self, pin: PinId) -> Option<bool> {
        match self {
            LogicComponent::Gate(gate) => gate.inputs.get(pin).cloned().flatten(),
            LogicComponent::Circuit(circuit) => {
                if let Some(gate_id) = circuit.input_gates.get(&pin) {
                    if let Some(gate) = circuit.components.get(gate_id) {
                        return gate.get_input(0);
                    }
                }
                None
            }
        }
    }

    pub fn get_output(&self, pin: PinId) -> Option<bool> {
        match self {
            LogicComponent::Gate(gate) => gate.output,
            LogicComponent::Circuit(circuit) => {
                if let Some(gate_id) = circuit.output_gates.get(&pin) {
                    if let Some(gate) = circuit.components.get(gate_id) {
                        return gate.get_output(0);
                    }
                }
                None
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum GateType {
    // Special gates
    Input, // Input gate, constant value
    // InputArray,  // Input gate, array of values
    InputPin, // Input port, value is set externally via a connection
    Output,   // Output gate, value is read internally
    // OutputArray, // Output gate, array of values
    OutputPin, // Output port, value is read externally via a connection

    // Oscillator
    Oscillator,

    // Circuit gate (collection of gates)
    Circuit,

    // Binary gates
    Or,
    And,

    // Unary gates
    Not,
    Identity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Circuit {
    pub id: Uuid,
    pub components: AHashMap<ComponentId, LogicComponent>,
    pub connections: Vec<Connection>,

    /* Topological sort for tick() and execute() */
    topological_order: Option<Vec<ComponentId>>,
    topological_dirty: bool,

    /* Port Config */
    input_gates: AHashMap<PinId, GateId>,
    output_gates: AHashMap<PinId, GateId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Gate {
    pub id: PortId,
    pub gate_type: GateType,
    pub gate_data: GateData,
    pub inputs: Vec<Option<bool>>,
    pub output: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum GateData {
    Oscillator { frequency: usize, tick: usize },
    None,
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub struct Connection {
    pub source: ComponentId,
    pub target: ComponentId,
    pub target_input: PinId,
    pub source_output: PinId,
}

pub trait CircuitTemplate {
    fn template() -> Circuit;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PortId {
    pub circuit_id: Uuid,
    pub gate_id: GateId,
}

pub struct GenericGate<L: GateLogic> {
    pub id: PortId,
    state: L::State,
    inputs: Vec<Option<bool>>,
    pub output: Option<bool>,
    _logic: PhantomData<L>,
}

// New gate lib
pub trait GateLogic {
    /// how many boolean inputs this gate reads
    const INPUTS: usize;
    /// any extra state (e.g. oscillator’s tick)
    type State: Default;
    /// advance the state and return the gate’s new output
    fn compute(state: &mut Self::State, inputs: &[bool]) -> bool;
}

impl<L: GateLogic> GenericGate<L> {
    pub fn new(circuit_id: Uuid) -> Self {
        GenericGate {
            id: PortId {
                circuit_id,
                gate_id: Uuid::new_v4(),
            },
            state: Default::default(),
            inputs: vec![None; L::INPUTS],
            output: None,
            _logic: PhantomData,
        }
    }

    pub fn compute(&mut self) {
        // only run if every input is Some(_)
        if self.inputs.iter().all(Option::is_some) {
            // unwrap into a Vec<bool>
            let ready: Vec<bool> = self.inputs.iter().map(|opt| opt.unwrap()).collect();

            // call the per‑gate logic
            let out = L::compute(&mut self.state, &ready);
            self.output = Some(out);
        }
    }

    pub fn set_input(&mut self, pin: PinId, value: Option<bool>) {
        if pin < self.inputs.len() {
            self.inputs[pin] = value;
        }
    }

    pub fn get_input(&self, pin: PinId) -> Option<bool> {
        self.inputs.get(pin).cloned().flatten()
    }

    pub fn get_output(&self) -> Option<bool> {
        self.output
    }
}

/// Input gate
pub struct InputLogic;
impl GateLogic for InputLogic {
    const INPUTS: usize = 1;
    type State = (); // no extra state
    fn compute(_: &mut (), arr: &[bool]) -> bool {
        arr[0]
    }
}

impl GenericGate<InputLogic> {
    /// create an input gate with a custom initial value
    pub fn with_initial(circuit_id: Uuid, initial: bool) -> Self {
        GenericGate {
            id: PortId {
                circuit_id,
                gate_id: Uuid::new_v4(),
            },
            state: (),
            inputs: vec![Some(initial)],
            output: None,
            _logic: PhantomData,
        }
    }
}

/// Output gate
pub struct OutputLogic;
impl GateLogic for OutputLogic {
    const INPUTS: usize = 1;
    type State = (); // no extra state
    fn compute(_: &mut (), arr: &[bool]) -> bool {
        arr[0]
    }
}

/// Identity gate
pub struct IdentityLogic;
impl GateLogic for IdentityLogic {
    const INPUTS: usize = 1;
    type State = (); // no extra state
    fn compute(_: &mut (), inputs: &[bool]) -> bool {
        debug_assert_eq!(
            inputs.len(),
            Self::INPUTS,
            "Identity gate requires exactly {} inputs, found: {}",
            Self::INPUTS,
            inputs.len()
        );
        inputs[0]
    }
}

/// A simple OR gate
pub struct OrLogic;
impl GateLogic for OrLogic {
    const INPUTS: usize = 2;
    type State = (); // no extra state
    fn compute(_: &mut (), inputs: &[bool]) -> bool {
        debug_assert_eq!(
            inputs.len(),
            Self::INPUTS,
            "OR gate requires exactly {} inputs, found: {}",
            Self::INPUTS,
            inputs.len()
        );
        inputs[0] || inputs[1]
    }
}

/// A simple NOT gate
pub struct NotLogic;
impl GateLogic for NotLogic {
    const INPUTS: usize = 1;
    type State = (); // no extra state
    fn compute(_: &mut (), inputs: &[bool]) -> bool {
        debug_assert_eq!(
            inputs.len(),
            Self::INPUTS,
            "NOT gate requires exactly {} inputs, found: {}",
            Self::INPUTS,
            inputs.len()
        );
        !inputs[0]
    }
}

/// A simple AND gate
pub struct AndLogic;
impl GateLogic for AndLogic {
    const INPUTS: usize = 2;
    type State = (); // no extra state
    fn compute(_: &mut (), inputs: &[bool]) -> bool {
        debug_assert_eq!(
            inputs.len(),
            Self::INPUTS,
            "AND gate requires exactly {} inputs, found: {}",
            Self::INPUTS,
            inputs.len()
        );
        inputs[0] && inputs[1]
    }
}

/// A toggling oscillator
pub struct OscillatorLogic;
pub struct OscillatorState {
    pub frequency: usize,
    pub tick: usize,
}
impl Default for OscillatorState {
    fn default() -> Self {
        OscillatorState {
            frequency: 1,
            tick: 0,
        }
    }
}

impl GateLogic for OscillatorLogic {
    const INPUTS: usize = 1;
    type State = OscillatorState; // the current tick
    fn compute(state: &mut Self::State, inputs: &[bool]) -> bool {
        debug_assert_eq!(
            inputs.len(),
            Self::INPUTS,
            "Oscillator gate requires exactly {} inputs, found: {}",
            Self::INPUTS,
            inputs.len()
        );
        state.tick = (state.tick + 1) % Self::INPUTS; // you could stash freq in State instead
        if state.tick == 0 {
            !inputs[0]
        } else {
            inputs[0]
        }
    }
}

impl GenericGate<OscillatorLogic> {
    /// create an oscillator gate with a custom frequency
    pub fn with_frequency(circuit_id: Uuid, frequency: usize) -> Self {
        GenericGate {
            id: PortId {
                circuit_id,
                gate_id: Uuid::new_v4(),
            },
            state: OscillatorState { frequency, tick: 0 },
            inputs: vec![None; OscillatorLogic::INPUTS],
            output: None,
            _logic: PhantomData,
        }
    }
}

pub enum AnyGate {
    Input(GenericGate<InputLogic>),
    Output(GenericGate<OutputLogic>),
    Identity(GenericGate<IdentityLogic>),
    Or(GenericGate<OrLogic>),
    Not(GenericGate<NotLogic>),
    And(GenericGate<AndLogic>),
    Oscillator(GenericGate<OscillatorLogic>),
}

impl AnyGate {
    pub fn new(gate_type: GateType, circuit_id: Uuid) -> Self {
        match gate_type {
            GateType::And => AnyGate::And(GenericGate::<AndLogic>::new(circuit_id)),
            GateType::Or => AnyGate::Or(GenericGate::<OrLogic>::new(circuit_id)),
            GateType::Not => AnyGate::Not(GenericGate::<NotLogic>::new(circuit_id)),
            GateType::Identity => AnyGate::Identity(GenericGate::<IdentityLogic>::new(circuit_id)),
            GateType::Input => AnyGate::Input(GenericGate::<InputLogic>::new(circuit_id)),
            GateType::Output => AnyGate::Output(GenericGate::<OutputLogic>::new(circuit_id)),
            GateType::Oscillator => {
                AnyGate::Oscillator(GenericGate::<OscillatorLogic>::new(circuit_id))
            }
            GateType::Circuit => {
                panic!("Use Circuit::new() to make a sub‐circuit");
            }
            _ => {
                unimplemented!("Ports & arrays not yet supported in AnyGate::new");
            }
        }
    }

    pub fn new_input(circuit_id: Uuid, initial_value: bool) -> Self {
        AnyGate::Input(GenericGate::<InputLogic>::with_initial(
            circuit_id,
            initial_value,
        ))
    }

    pub fn new_output(circuit_id: Uuid) -> Self {
        AnyGate::Output(GenericGate::<OutputLogic>::new(circuit_id))
    }

    pub fn new_identity(circuit_id: Uuid) -> Self {
        AnyGate::Identity(GenericGate::<IdentityLogic>::new(circuit_id))
    }

    pub fn new_oscillator(circuit_id: Uuid, frequency: usize) -> Self {
        AnyGate::Oscillator(GenericGate::<OscillatorLogic>::with_frequency(
            circuit_id, frequency,
        ))
    }

    pub fn new_and(circuit_id: Uuid) -> Self {
        AnyGate::And(GenericGate::<AndLogic>::new(circuit_id))
    }

    pub fn new_or(circuit_id: Uuid) -> Self {
        AnyGate::Or(GenericGate::<OrLogic>::new(circuit_id))
    }

    pub fn new_not(circuit_id: Uuid) -> Self {
        AnyGate::Not(GenericGate::<NotLogic>::new(circuit_id))
    }

    pub fn get_gate_type(&self) -> GateType {
        match self {
            AnyGate::Input(_) => GateType::Input,
            AnyGate::Output(_) => GateType::Output,
            AnyGate::Identity(_) => GateType::Identity,
            AnyGate::Or(_) => GateType::Or,
            AnyGate::Not(_) => GateType::Not,
            AnyGate::And(_) => GateType::And,
            AnyGate::Oscillator(_) => GateType::Oscillator,
        }
    }

    pub fn get_id(&self) -> PortId {
        match self {
            AnyGate::Input(gate) => gate.id,
            AnyGate::Output(gate) => gate.id,
            AnyGate::Identity(gate) => gate.id,
            AnyGate::Or(gate) => gate.id,
            AnyGate::Not(gate) => gate.id,
            AnyGate::And(gate) => gate.id,
            AnyGate::Oscillator(gate) => gate.id,
        }
    }
}
