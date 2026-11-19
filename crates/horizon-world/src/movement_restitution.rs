//! Native post-collision restitution, including lazy bounce queries and effects.
use crate::registry::BlockStateId;
use horizon_core::{geometry::Vec3, position::BlockPos};

pub struct RestitutionInput {
    pub block: BlockStateId,
    pub collided_x: bool,
    pub collided_z: bool,
    pub vertical_collision: bool,
    pub collision_below: bool,
    pub living: bool,
    pub resolved_movement: Vec3,
    pub fall_distance: f64,
}
pub trait RestitutionWorld {
    type Error;
    fn uses_block_landing_12111(&self) -> bool {
        false
    }
    fn suppressing_bounce(&mut self) -> Result<bool, Self::Error>;
    fn entity_bounciness(&mut self) -> Result<f64, Self::Error>;
    fn velocity(&mut self) -> Result<Vec3, Self::Error>;
    fn effective_gravity(&mut self) -> Result<f64, Self::Error>;
    fn block_suppresses_bounce(&mut self, block: BlockStateId) -> Result<bool, Self::Error>;
    fn block_bounce_restitution(&mut self, block: BlockStateId) -> Result<f32, Self::Error>;
    fn air_drag(&mut self) -> Result<f32, Self::Error>;
    fn movement_support_position(&mut self) -> Result<BlockPos, Self::Error>;
    fn bounce_on(
        &mut self,
        block: BlockStateId,
        position: BlockPos,
        fall_distance: f64,
    ) -> Result<(), Self::Error>;
    fn bounce_event(&mut self) -> Result<(), Self::Error>;
    /// Native field write after bounce callbacks and before setting velocity.
    fn mark_position_for_sync(&mut self);
    fn set_velocity(&mut self, velocity: Vec3) -> Result<(), Self::Error>;
}
fn java_max(a: f64, b: f64) -> f64 {
    if a.is_nan() {
        a
    } else if b.is_nan() {
        b
    } else if a == 0.0 && b == 0.0 {
        f64::from_bits(a.to_bits() & b.to_bits())
    } else {
        a.max(b)
    }
}
/// Provider errors preserve already performed effects; no rollback is attempted.
/// Call only when Entity.move's simulation/collision guards request restitution.
pub fn restitute_movement<W: RestitutionWorld>(
    world: &mut W,
    input: RestitutionInput,
) -> Result<(), W::Error> {
    if world.uses_block_landing_12111() {
        let mut velocity = world.velocity()?;
        if input.collided_x {
            velocity.x = 0.0;
        }
        if input.collided_z {
            velocity.z = 0.0;
        }
        if input.vertical_collision {
            let bounce = world.block_bounce_restitution(input.block)?;
            let suppressed = bounce != 0.0 && world.suppressing_bounce()?;
            velocity.y = block_landing_velocity_12111(velocity.y, bounce, suppressed, input.living);
        }
        return world.set_velocity(velocity);
    }

    let mut bounce = if world.suppressing_bounce()? {
        0.0
    } else {
        world.entity_bounciness()?
    };
    let old = world.velocity()?;
    let mut velocity = old;
    if input.collided_x {
        velocity.x = -old.x * bounce;
    }
    if input.collided_z {
        velocity.z = -old.z * bounce;
    }
    let mut emit = bounce > 0.0 && (input.collided_x || input.collided_z);
    if input.vertical_collision {
        if input.collision_below {
            if -old.y <= world.effective_gravity()?
                || world.suppressing_bounce()?
                || world.block_suppresses_bounce(input.block)?
            {
                bounce = 0.0;
            } else {
                let mut block = world.block_bounce_restitution(input.block)?;
                if !input.living {
                    block *= 0.8f32;
                }
                bounce = java_max(bounce, f64::from(block));
            }
        }
        let (gravity, drag) = if bounce > 0.0 {
            let fraction = input.resolved_movement.y / old.y;
            let gravity = fraction * world.effective_gravity()?;
            let drag = 1.0 + fraction * (f64::from(world.air_drag()?) - 1.0);
            emit = true;
            (gravity, drag)
        } else {
            (0.0, 1.0)
        };
        velocity.y = (gravity - old.y) * drag * bounce;
    }
    if emit {
        let position = world.movement_support_position()?;
        world.bounce_on(input.block, position, input.fall_distance)?;
        world.bounce_event()?;
        world.mark_position_for_sync();
    }
    world.set_velocity(velocity)
}

// The 1.21.11 block callback acts directly on velocity, including very small falls.
fn block_landing_velocity_12111(y: f64, bounce: f32, suppressed: bool, living: bool) -> f64 {
    if bounce == 0.0 || suppressed {
        y * 0.0
    } else if y < 0.0 {
        -y * f64::from(bounce) * if living { 1.0 } else { 0.8 }
    } else {
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_block_callbacks_handle_beds_slime_and_sneaking() {
        assert_eq!(block_landing_velocity_12111(-0.01, 1.0, false, true), 0.01);
        assert_eq!(block_landing_velocity_12111(-1.0, 1.0, false, false), 0.8);
        assert_eq!(
            block_landing_velocity_12111(-1.0, 0.66, false, false),
            0.528000020980835
        );
        assert_eq!(block_landing_velocity_12111(0.5, 0.66, false, true), 0.5);
        for (block, sneak) in [(0.0, false), (1.0, true), (0.66, true)] {
            assert_eq!(
                block_landing_velocity_12111(-1.0, block, sneak, true).to_bits(),
                (-0.0_f64).to_bits()
            );
        }
    }
}
