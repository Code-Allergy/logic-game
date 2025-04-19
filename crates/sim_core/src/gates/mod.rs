use crate::{ComponentId, Connection, PinId, PortId};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::marker::PhantomData;
use uuid::Uuid;

pub mod proto;

impl Connection {
    pub fn new(
        source: ComponentId,
        target: ComponentId,
        target_input: PinId,
        source_output: PinId,
    ) -> Self {
        Connection {
            source,
            source_output,
            target,
            target_input,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenericGate<L: GateLogic> {
    pub id: PortId,
    state: L::State,
    inputs: Vec<Option<bool>>,
    pub output: Option<bool>,
    _logic: PhantomData<L>,
}

// New gate lib
pub trait GateLogic: Debug {
    /// how many boolean inputs this gate reads
    const INPUTS: usize;
    /// any extra state (e.g. oscillator’s tick)
    type State: Default + Debug;
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
        // Verify that the number of inputs is correct
        debug_assert_eq!(
            self.inputs.len(),
            L::INPUTS,
            "Gate {} has {} inputs, but logic requires {}",
            self.id.gate_id,
            self.inputs.len(),
            L::INPUTS
        );

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
        } else {
            println!(
                "Trying to set input {} on gate with {} inputs",
                pin,
                self.inputs.len()
            );
            dbg!(&self);
            panic!("Pin index out of bounds: {} >= {}", pin, self.inputs.len());
        }
    }

    pub fn get_input(&self, pin: PinId) -> Option<bool> {
        self.inputs.get(pin).cloned().flatten()
    }

    pub fn get_output(&self) -> Option<bool> {
        self.output
    }

    pub fn expected_inputs(&self) -> usize {
        L::INPUTS
    }
}

/* IN/OUT gates */

/// Input gate
#[derive(Clone, Debug, Serialize, Deserialize)]

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

/// Input pin gate
/// This gate is used to add an input to a circuit.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputPinLogic {
    pub pin: PinId,
}

impl GateLogic for InputPinLogic {
    const INPUTS: usize = 1;
    type State = (); // no extra state
    fn compute(_: &mut (), arr: &[bool]) -> bool {
        arr[0]
    }
}

/// Output gate
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutputLogic;
impl GateLogic for OutputLogic {
    const INPUTS: usize = 1;
    type State = (); // no extra state
    fn compute(_: &mut (), arr: &[bool]) -> bool {
        arr[0]
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutputPinLogic {
    pub pin: PinId,
}
impl GateLogic for OutputPinLogic {
    const INPUTS: usize = 1;
    type State = (); // no extra state
    fn compute(_: &mut (), arr: &[bool]) -> bool {
        arr[0]
    }
}

/* Logic type gates */

/// A simple OR gate
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrLogic;
impl GateLogic for OrLogic {
    const INPUTS: usize = 2;
    type State = (); // no extra state
    fn compute(_: &mut Self::State, inputs: &[bool]) -> bool {
        inputs[0] || inputs[1]
    }
}

/// A simple AND gate
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AndLogic;
impl GateLogic for AndLogic {
    const INPUTS: usize = 2;
    type State = (); // no extra state
    fn compute(_: &mut (), inputs: &[bool]) -> bool {
        inputs[0] && inputs[1]
    }
}

/// Identity gate
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentityLogic;
impl GateLogic for IdentityLogic {
    const INPUTS: usize = 1;
    type State = (); // no extra state
    fn compute(_: &mut Self::State, inputs: &[bool]) -> bool {
        inputs[0]
    }
}

/// A simple NOT gate
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotLogic;
impl GateLogic for NotLogic {
    const INPUTS: usize = 1;
    type State = (); // no extra state
    fn compute(_: &mut (), inputs: &[bool]) -> bool {
        !inputs[0]
    }
}

/* Clock */

/// A toggling oscillator
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OscillatorLogic;

#[derive(Clone, Debug, Serialize, Deserialize)]
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
        state.tick = (state.tick + 1) % state.frequency;
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
