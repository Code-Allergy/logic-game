use crate::sim::GateComponent;
use crate::sim::Selected;
use crate::UiPointerState;
use bevy::input::mouse::MouseButton;
use bevy::prelude::*;

#[derive(Resource, Default)]
pub struct SelectionState {
    pub drag_start: Option<Vec2>,
    pub drag_current: Option<Vec2>,
}

#[derive(Message, Debug)]
pub struct ComponentsSelected {
    pub entities: Vec<Entity>,
}

#[derive(Message, Debug)]
pub struct ComponentsUnselected {
    pub entities: Vec<Entity>,
}

#[derive(Component)]
pub struct SelectionBox;

pub fn mouse_selection_input(
    ui_pointer: Res<UiPointerState>,
    mouse_input: Res<ButtonInput<MouseButton>>,
    mut cursor_moved: MessageReader<CursorMoved>,
    mut sel: ResMut<SelectionState>,
) {
    if ui_pointer.over_egui {
        if mouse_input.just_released(MouseButton::Left) {
            sel.drag_start = None;
            sel.drag_current = None;
        }
        return;
    }

    // 1) Update the latest cursor position every frame
    let mut latest_pos = None;
    for ev in cursor_moved.read() {
        latest_pos = Some(ev.position);
    }
    let pos = if let Some(p) = latest_pos { p } else { return };

    // 2) On left-button press, record drag start
    if mouse_input.just_pressed(MouseButton::Left) {
        info!("Left mouse button pressed at {:?}", pos);
        sel.drag_start = Some(pos);
        sel.drag_current = Some(pos);
    }
    // 3) While held, update drag_current
    else if mouse_input.pressed(MouseButton::Left) {
        if sel.drag_start.is_some() {
            sel.drag_current = Some(pos);
        }
    }
    // 4) On release, leave detection to a separate system
    else if mouse_input.just_released(MouseButton::Left) {
        info!("Left mouse button released at {:?}", pos);
        // keep sel.drag_start/current for finalize_selection()
    }
}

pub fn draw_selection_box(
    mut commands: Commands,
    sel: Res<SelectionState>,
    selection_box_query: Query<Entity, With<SelectionBox>>,
) {
    // Clear any existing selection box
    for entity in selection_box_query.iter() {
        commands.entity(entity).despawn();
    }

    // Draw new selection box if user is dragging
    if let (Some(start), Some(current)) = (sel.drag_start, sel.drag_current) {
        let min = Vec2::new(start.x.min(current.x), start.y.min(current.y));
        let max = Vec2::new(start.x.max(current.x), start.y.max(current.y));
        let size = max - min;
        let center = min + size / 2.0;

        // Only draw if there's a meaningful drag (not just a click)
        if size.length() > 5.0 {
            commands.spawn((
                Sprite {
                    color: Color::srgba(0.3, 0.5, 1.0, 0.4),
                    custom_size: Some(size),
                    ..Default::default()
                },
                Transform::from_xyz(center.x, center.y, 100.0),
                SelectionBox,
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn finalize_selection(
    mut sel: ResMut<SelectionState>,
    ui_pointer: Res<UiPointerState>,
    camera_q: Query<(&Camera, &GlobalTransform)>,
    mut select_writer: MessageWriter<ComponentsSelected>,
    mut unselect_writer: MessageWriter<ComponentsUnselected>,
    query: Query<(Entity, &GlobalTransform), With<GateComponent>>,
    selection_box_query: Query<Entity, With<SelectionBox>>,
    mut commands: Commands,
    selected_query: Query<Entity, With<Selected>>,
) -> Result {
    // Only act on release
    if ui_pointer.over_egui {
        sel.drag_start = None;
        sel.drag_current = None;
        return Ok(());
    }

    if sel.drag_start.is_none() || sel.drag_current.is_none() {
        return Ok(());
    }

    let (camera, cam_tf) = camera_q.single()?;

    // Build screen-space rectangle
    let a = sel.drag_start.unwrap();
    let b = sel.drag_current.unwrap();
    let min = Vec2::new(a.x.min(b.x), a.y.min(b.y));
    let max = Vec2::new(a.x.max(b.x), a.y.max(b.y));

    // Don't process if it's a small drag (likely just a click)
    let selection_size = max - min;
    if selection_size.length() < 5.0 {
        sel.drag_start = None;
        sel.drag_current = None;
        return Ok(());
    }

    // Gather entities inside the rectangle
    let mut selected = Vec::new();
    for (ent, tf) in query.iter() {
        // project world → viewport coords
        if let Ok(screen_pos) = camera.world_to_viewport(cam_tf, tf.translation())
            && screen_pos.x >= min.x
            && screen_pos.x <= max.x
            && screen_pos.y >= min.y
            && screen_pos.y <= max.y
        {
            selected.push(ent);
        }
    }

    // Compile a list of entities to unselect (those currently selected but not in new selection)
    let mut unselected = Vec::new();
    for entity in selected_query.iter() {
        if !selected.contains(&entity) {
            unselected.push(entity);
        }
    }

    // Emit selection events
    if !selected.is_empty() {
        info!("Selected entities: {:?}", selected);
        select_writer.write(ComponentsSelected { entities: selected });
    }

    if !unselected.is_empty() {
        unselect_writer.write(ComponentsUnselected {
            entities: unselected,
        });
    }

    // Clean up selection box
    for entity in selection_box_query.iter() {
        commands.entity(entity).despawn();
    }

    // Clear the drag state
    sel.drag_start = None;
    sel.drag_current = None;
    Ok(())
}

// System to apply selection to entities
pub fn apply_selection(
    mut commands: Commands,
    mut selection_event: MessageReader<ComponentsSelected>,
) {
    for event in selection_event.read() {
        for &entity in &event.entities {
            commands
                .entity(entity)
                .insert(Selected { offset: Vec2::ZERO });
        }
    }
}

// System to remove selection from entities
pub fn remove_selection(
    mut commands: Commands,
    mut unselection_event: MessageReader<ComponentsUnselected>,
) {
    for event in unselection_event.read() {
        for &entity in &event.entities {
            commands.entity(entity).remove::<Selected>();
        }
    }
}

pub struct SelectionPlugin;

impl Plugin for SelectionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SelectionState>()
            .add_message::<ComponentsSelected>()
            .add_message::<ComponentsUnselected>()
            .add_systems(
                Update,
                (
                    mouse_selection_input,
                    draw_selection_box,
                    finalize_selection,
                    // apply_selection,
                    // remove_selection,
                ),
            );
    }
}
