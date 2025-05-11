// use crate::sim::GateComponent;
// use bevy::input::mouse::MouseButton;
// use bevy::prelude::*;
// use bevy::window::PrimaryWindow;

// #[derive(Resource, Default)]
// pub struct SelectionState {
//     pub drag_start: Option<Vec2>,
//     pub drag_current: Option<Vec2>,
// }
// pub fn mouse_selection_input(
//     mouse_input: Res<ButtonInput<MouseButton>>,
//     mut cursor_moved: EventReader<CursorMoved>,
//     mut sel: ResMut<SelectionState>,
// ) {
//     // 1) Update the latest cursor position every frame
//     let mut latest_pos = None;
//     for ev in cursor_moved.read() {
//         latest_pos = Some(ev.position);
//     }
//     let pos = if let Some(p) = latest_pos { p } else { return };

//     // 2) On right‑button press, record drag start
//     if mouse_input.just_pressed(MouseButton::Left) {
//         info!("Right mouse button pressed at {:?}", pos);
//         sel.drag_start = Some(pos);
//         sel.drag_current = Some(pos);
//     }
//     // 3) While held, update drag_current
//     else if mouse_input.pressed(MouseButton::Left) {
//         info!("Right mouse button held at {:?}", pos);
//         if sel.drag_start.is_some() {
//             sel.drag_current = Some(pos);
//         }
//     }
//     // 4) On release, leave detection to a separate system
//     else if mouse_input.just_released(MouseButton::Left) {
//         info!("Right mouse button released at {:?}", pos);
//         // keep sel.drag_start/current for finalize_selection()
//     }
// }

// pub fn finalize_selection(
//     mut sel: ResMut<SelectionState>,
//     camera_q: Query<(&Camera, &GlobalTransform)>,
//     windows: Query<&Window, With<PrimaryWindow>>,
//     // mut select_writer: EventWriter<ComponentsSelected>,
//     // mut unselect_writer: EventWriter<ComponentsUnselected>,
//     query: Query<(Entity, &GlobalTransform), With<GateComponent>>,
// ) {
//     // Only act on release
//     if sel.drag_start.is_none() || sel.drag_current.is_none() {
//         return;
//     }

//     let (camera, cam_tf) = camera_q.single();
//     let window = windows.single();

//     // Build screen‑space rectangle
//     let a = sel.drag_start.unwrap();
//     let b = sel.drag_current.unwrap();
//     let min = Vec2::new(a.x.min(b.x), a.y.min(b.y));
//     let max = Vec2::new(a.x.max(b.x), a.y.max(b.y));

//     // Gather entities inside the rectangle
//     let mut selected = Vec::new();
//     for (ent, tf) in query.iter() {
//         // project world → viewport coords
//         if let Ok(screen_pos) = camera.world_to_viewport(cam_tf, tf.translation()) {
//             if screen_pos.x >= min.x
//                 && screen_pos.x <= max.x
//                 && screen_pos.y >= min.y
//                 && screen_pos.y <= max.y
//             {
//                 selected.push(ent);
//             }
//         }
//     }
//     dbg!(&selected);

//     // // Emit your selection events
//     // select_writer.send(ComponentsSelected {
//     //     entities: selected.clone(),
//     // });
//     // // Optionally compute and send unselected if you track previous selection...
//     // unselect_writer.send(ComponentsUnselected { entities: vec![] });

//     // Clear the drag state
//     sel.drag_start = None;
//     sel.drag_current = None;
// }

// pub struct SelectionPlugin;

// impl Plugin for SelectionPlugin {
//     fn build(&self, _app: &mut App) {
//         // app //.add_event::<ComponentsSelected>()
//         //.add_event::<ComponentsUnselected>()
//         // .init_resource();
//         // .add_systems(Update)
//         // .add_systems(Update);
//     }
// }
