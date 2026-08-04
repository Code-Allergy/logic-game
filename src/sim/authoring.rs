//! ECS authoring model for circuits.
//!
//! These components are the live, UUID-addressed source of truth. `GameCircuit`
//! remains a portable document/bridge snapshot and is generated from this data.

use std::collections::HashMap;

use bevy::prelude::*;
use uuid::Uuid;

use super::{GameCircuit, GameNode, GameNodeKind, GameWire, GateKindTag, WireKind};

pub const DEFAULT_LAYER_UUID: Uuid = Uuid::from_u128(1);

#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource)]
pub struct ActiveCircuit {
    pub uuid: Uuid,
    pub name: String,
}

impl ActiveCircuit {
    pub fn from_circuit(circuit: &GameCircuit) -> Self {
        Self { uuid: circuit.id, name: circuit.name.clone() }
    }
}

#[derive(Resource, Debug, Default, Reflect)]
#[reflect(Resource)]
pub struct WorkspaceRevision {
    pub current: u64,
    pub saved: u64,
}

impl WorkspaceRevision {
    pub fn mark_dirty(&mut self) { self.current = self.current.wrapping_add(1); }
    pub fn mark_saved(&mut self) { self.saved = self.current; }
    pub fn is_dirty(&self) -> bool { self.current != self.saved }
}

#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct CircuitMember { pub circuit_uuid: Uuid }

#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct CircuitLayer {
    pub layer_uuid: Uuid,
    pub order: i32,
}

impl Default for CircuitLayer {
    fn default() -> Self { Self { layer_uuid: DEFAULT_LAYER_UUID, order: 0 } }
}

#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct GateIdentity { pub uuid: Uuid }

#[derive(Component, Debug, Clone, Reflect)]
#[reflect(Component)]
pub struct GateLabel { pub value: String }

#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct GateType { pub kind: GateKindTag }

#[derive(Component, Debug, Clone, Reflect)]
#[reflect(Component)]
pub struct GatePorts {
    pub input_widths: Vec<u8>,
    pub output_widths: Vec<u8>,
}

#[derive(Component, Debug, Clone, Copy, Default, Reflect)]
#[reflect(Component)]
pub struct GatePinState {
    pub input_bits: u64,
    pub output_bits: u64,
}

#[derive(Component, Debug, Clone, Reflect)]
#[reflect(Component)]
pub struct PersistentGateData {
    #[reflect(ignore)]
    pub custom_properties: HashMap<String, String>,
}

#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct ClockConfig {
    pub period_ticks: u32,
    pub duty_ticks: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Reflect)]
pub enum DisplayRadix { Binary, Decimal, #[default] Hexadecimal }

#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct DisplayConfig {
    pub bits: u8,
    pub radix: DisplayRadix,
    pub leading_zeroes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Reflect)]
pub enum PortDirection { Input, Output }

#[derive(Debug, Clone, Reflect)]
pub struct InterfacePort {
    pub uuid: Uuid,
    pub label: String,
    pub direction: PortDirection,
    pub width: u8,
    pub ordinal: u16,
}

#[derive(Component, Debug, Clone, Reflect)]
#[reflect(Component)]
pub struct SubCircuitInstance {
    pub definition_uuid: Uuid,
    pub instance_uuid: Uuid,
    pub definition_name: String,
    pub input_count: u16,
    pub output_count: u16,
    pub nesting_depth: u16,
    pub interface_ports: Vec<InterfacePort>,
    #[reflect(ignore)]
    pub definition: Box<GameCircuit>,
}

#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct WireIdentity { pub uuid: Uuid }

#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct WireEndpoints {
    pub source_gate: Uuid,
    pub source_pin: u8,
    pub sink_gate: Uuid,
    pub sink_pin: u8,
}

#[derive(Component, Debug, Clone, Reflect)]
#[reflect(Component)]
pub struct WireProperties {
    pub width: u8,
    #[reflect(ignore)]
    pub kind: WireKind,
}

#[derive(Component, Debug, Clone, Copy, Default, Reflect)]
#[reflect(Component)]
pub struct WireState { pub value: u64 }

#[derive(Bundle)]
pub struct GateAuthoringBundle {
    pub identity: GateIdentity,
    pub label: GateLabel,
    pub gate_type: GateType,
    pub ports: GatePorts,
    pub state: GatePinState,
    pub member: CircuitMember,
    pub layer: CircuitLayer,
    pub persistent: PersistentGateData,
}

impl GateAuthoringBundle {
    pub fn new(node: &GameNode, circuit_uuid: Uuid) -> Self {
        let input_widths = (0..node.kind.input_count())
            .map(|pin| node.kind.input_pin_width(pin as u8).unwrap_or(1))
            .collect();
        let output_widths = (0..node.kind.output_count())
            .map(|pin| node.kind.output_pin_width(pin as u8).unwrap_or(1))
            .collect();
        let layer = CircuitLayer {
            layer_uuid: node.custom_properties.get("gatequest.layer_uuid")
                .and_then(|value| Uuid::parse_str(value).ok())
                .unwrap_or(DEFAULT_LAYER_UUID),
            order: node.custom_properties.get("gatequest.layer_order")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
        };
        Self {
            identity: GateIdentity { uuid: node.id },
            label: GateLabel { value: node.label.clone().unwrap_or_default() },
            gate_type: GateType { kind: GateKindTag::from_node_kind(&node.kind) },
            ports: GatePorts { input_widths, output_widths },
            state: GatePinState::default(),
            member: CircuitMember { circuit_uuid },
            layer,
            persistent: PersistentGateData { custom_properties: node.custom_properties.clone() },
        }
    }
}

#[derive(Bundle)]
pub struct WireAuthoringBundle {
    pub identity: WireIdentity,
    pub endpoints: WireEndpoints,
    pub properties: WireProperties,
    pub state: WireState,
    pub member: CircuitMember,
    pub layer: CircuitLayer,
}

impl WireAuthoringBundle {
    pub fn new(wire: &GameWire, circuit_uuid: Uuid) -> Self {
        Self {
            identity: WireIdentity { uuid: wire.id },
            endpoints: WireEndpoints {
                source_gate: wire.source_node,
                source_pin: wire.source_pin,
                sink_gate: wire.sink_node,
                sink_pin: wire.sink_pin,
            },
            properties: WireProperties { width: wire.kind.bit_width(), kind: wire.kind.clone() },
            state: WireState::default(),
            member: CircuitMember { circuit_uuid },
            layer: CircuitLayer {
                layer_uuid: if wire.layer_uuid.is_nil() { DEFAULT_LAYER_UUID } else { wire.layer_uuid },
                order: wire.layer_order,
            },
        }
    }
}

pub fn configuration_components(node: &GameNode) -> (Option<ClockConfig>, Option<DisplayConfig>, Option<SubCircuitInstance>) {
    match &node.kind {
        GameNodeKind::Clock { period_ticks, duty_ticks } => (
            Some(ClockConfig { period_ticks: *period_ticks, duty_ticks: *duty_ticks }), None, None,
        ),
        GameNodeKind::Display { bits } => (
            None, Some(DisplayConfig { bits: *bits, radix: DisplayRadix::Hexadecimal, leading_zeroes: true }), None,
        ),
        GameNodeKind::SubCircuit { circuit } => {
            let mut ports = Vec::new();
            let mut input_ordinal = 0_u16;
            let mut output_ordinal = 0_u16;
            for interface in circuit.nodes.values() {
                let (direction, width, ordinal) = match interface.kind {
                    GameNodeKind::Input | GameNodeKind::Input8 | GameNodeKind::Input64 => {
                        let ordinal = input_ordinal; input_ordinal += 1;
                        (PortDirection::Input, interface.kind.output_count() as u8, ordinal)
                    }
                    GameNodeKind::Output | GameNodeKind::Output8 | GameNodeKind::Output64 => {
                        let ordinal = output_ordinal; output_ordinal += 1;
                        (PortDirection::Output, interface.kind.input_count() as u8, ordinal)
                    }
                    _ => continue,
                };
                ports.push(InterfacePort {
                    uuid: interface.id,
                    label: interface.label.clone().unwrap_or_default(),
                    direction,
                    width,
                    ordinal,
                });
            }
            (None, None, Some(SubCircuitInstance {
                definition_uuid: circuit.template_uuid(),
                instance_uuid: circuit.id,
                definition_name: circuit.name.clone(),
                input_count: input_ordinal,
                output_count: output_ordinal,
                nesting_depth: circuit_nesting_depth(circuit),
                interface_ports: ports,
                definition: circuit.clone(),
            }))
        }
        _ => (None, None, None),
    }
}

fn circuit_nesting_depth(circuit: &GameCircuit) -> u16 {
    circuit.nodes.values().filter_map(|node| match &node.kind {
        GameNodeKind::SubCircuit { circuit } => Some(circuit_nesting_depth(circuit)),
        _ => None,
    }).max().unwrap_or(0).saturating_add(1)
}

pub fn node_kind_from_components(
    gate_type: &GateType,
    clock: Option<&ClockConfig>,
    display: Option<&DisplayConfig>,
    subcircuit: Option<&SubCircuitInstance>,
) -> GameNodeKind {
    match gate_type.kind {
        GateKindTag::Input => GameNodeKind::Input,
        GateKindTag::Input8 => GameNodeKind::Input8,
        GateKindTag::Input64 => GameNodeKind::Input64,
        GateKindTag::Output => GameNodeKind::Output,
        GateKindTag::Output8 => GameNodeKind::Output8,
        GateKindTag::Output64 => GameNodeKind::Output64,
        GateKindTag::Display => GameNodeKind::Display { bits: display.map_or(1, |value| value.bits) },
        GateKindTag::Expand8 => GameNodeKind::Expand8,
        GateKindTag::Expand64 => GameNodeKind::Expand64,
        GateKindTag::ReduceOr8 => GameNodeKind::ReduceOr8,
        GateKindTag::ReduceOr64 => GameNodeKind::ReduceOr64,
        GateKindTag::And => GameNodeKind::And,
        GateKindTag::Or => GameNodeKind::Or,
        GateKindTag::Not => GameNodeKind::Not,
        GateKindTag::Xor => GameNodeKind::Xor,
        GateKindTag::Nand => GameNodeKind::Nand,
        GateKindTag::Nor => GameNodeKind::Nor,
        GateKindTag::Xnor => GameNodeKind::Xnor,
        GateKindTag::Buffer => GameNodeKind::Buffer,
        GateKindTag::Mux => GameNodeKind::Mux,
        GateKindTag::Demux => GameNodeKind::Demux,
        GateKindTag::TriStateBuffer => GameNodeKind::TriStateBuffer,
        GateKindTag::Counter8 => GameNodeKind::Counter8,
        GateKindTag::Clock => GameNodeKind::Clock {
            period_ticks: clock.map_or(10, |value| value.period_ticks),
            duty_ticks: clock.map_or(5, |value| value.duty_ticks),
        },
        GateKindTag::SubCircuit => GameNodeKind::SubCircuit {
            circuit: subcircuit.map_or_else(|| Box::new(GameCircuit::default()), |value| value.definition.clone()),
        },
    }
}

#[derive(Message)]
pub struct GateLabelChanged { pub gate_uuid: Uuid, pub label: String }

#[derive(Message)]
pub struct ClockConfigChanged { pub gate_uuid: Uuid, pub period_ticks: u32, pub duty_ticks: u32 }

#[derive(Message)]
pub struct DisplayConfigChanged { pub gate_uuid: Uuid, pub bits: u8, pub radix: DisplayRadix, pub leading_zeroes: bool }

#[derive(Message)]
pub struct GateMoved { pub gate_uuid: Uuid, pub position: Vec2 }

#[derive(Message)]
pub struct EntityLayerChanged { pub entity_uuid: Uuid, pub layer_uuid: Uuid, pub order: i32 }

#[derive(Message)]
pub struct AuthoringTopologyChanged;
