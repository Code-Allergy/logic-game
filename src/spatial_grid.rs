use bevy::{platform::collections::HashMap, prelude::*};

// Define a 2D grid for spatial partitioning
#[derive(Resource, Default)]
pub struct SpatialGrid {
    // Map from grid cell to entities in that cell
    cells: HashMap<(i32, i32), Vec<Entity>>,
    // Cell size for the grid
    cell_size: f32,
}

impl SpatialGrid {
    pub fn new(cell_size: f32) -> Self {
        SpatialGrid {
            cells: HashMap::new(),
            cell_size,
        }
    }

    // Convert world position to grid cell
    fn get_cell(&self, position: Vec2) -> (i32, i32) {
        (
            (position.x / self.cell_size).floor() as i32,
            (position.y / self.cell_size).floor() as i32,
        )
    }

    // Add entity to the grid
    pub fn insert(&mut self, entity: Entity, position: Vec2) {
        let cell = self.get_cell(position);
        self.cells.entry(cell).or_default().push(entity);
    }

    // Remove entity from the grid
    pub fn remove(&mut self, entity: Entity, position: Vec2) {
        let cell = self.get_cell(position);
        if let Some(entities) = self.cells.get_mut(&cell) {
            entities.retain(|&e| e != entity);
        }
    }

    // Update entity position in the grid
    pub fn update(&mut self, entity: Entity, old_pos: Vec2, new_pos: Vec2) {
        self.remove(entity, old_pos);
        self.insert(entity, new_pos);
    }

    // Query entities near a position
    pub fn query_near(&self, position: Vec2, radius: f32) -> Vec<Entity> {
        let mut result = Vec::new();
        let cell = self.get_cell(position);

        // Calculate how many cells we need to check based on radius
        let cell_radius = (radius / self.cell_size).ceil() as i32;

        // Check nearby cells
        for i in -cell_radius..=cell_radius {
            for j in -cell_radius..=cell_radius {
                let query_cell = (cell.0 + i, cell.1 + j);
                if let Some(entities) = self.cells.get(&query_cell) {
                    result.extend(entities);
                }
            }
        }

        result
    }

    // Clear the grid
    pub fn clear(&mut self) {
        self.cells.clear();
    }
}
