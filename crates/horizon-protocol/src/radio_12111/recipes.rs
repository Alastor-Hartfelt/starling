//! Protocol 774 slot displays use older type IDs and ordinary item stacks.
use super::{BlockStateTarget, item_registry_id, item_types, read_item_slot};
use crate::{
    Error, Result,
    buf::Reader,
    ids::play::clientbound,
    packets::{
        play::Clientbound,
        recipes::{
            HolderSet, PlaceGhostRecipe, RecipeBookAdd, RecipeBookEntry, RecipeDisplay, SlotDisplay,
            TrimPattern,
        },
    },
};

fn item(id: i32, target: BlockStateTarget) -> Result<i32> {
    match target {
        BlockStateTarget::Engine26_3 => item_registry_id(id, &item_types::ITEMS),
        BlockStateTarget::Native12111 => super::native_id(id, item_types::ITEMS.len())
            .ok_or_else(|| Error::new("unknown 1.21.11 recipe item")),
    }
}

fn slot(r: &mut Reader, target: BlockStateTarget, depth: u8) -> Result<SlotDisplay> {
    if depth > 32 {
        return Err(Error::new("1.21.11 slot displays nest too deep"));
    }
    let inner = |r: &mut Reader| slot(r, target, depth + 1).map(Box::new);
    Ok(match r.varint()? {
        0 => SlotDisplay::Empty,
        1 => SlotDisplay::AnyFuel,
        2 => SlotDisplay::Item(item(r.varint()?, target)?),
        3 => match read_item_slot(r, target)? {
            Some(s) => SlotDisplay::ItemStack(s),
            None => SlotDisplay::Empty,
        },
        4 => SlotDisplay::Tag(HolderSet::Tag(r.string(32767)?)),
        5 => SlotDisplay::SmithingTrim {
            base: inner(r)?,
            material: inner(r)?,
            pattern: match r.varint()? {
                0 => TrimPattern::Direct {
                    asset: r.string(32767)?,
                    description: r.nbt()?,
                    decal: r.bool()?,
                },
                id if id > 0 => TrimPattern::Id(id - 1),
                _ => return Err(Error::new("invalid recipe trim pattern")),
            },
        },
        6 => SlotDisplay::WithRemainder {
            input: inner(r)?,
            remainder: inner(r)?,
        },
        7 => {
            let count = r.count(r.remaining())?;
            SlotDisplay::Composite(
                (0..count)
                    .map(|_| slot(r, target, depth + 1))
                    .collect::<Result<_>>()?,
            )
        }
        id => return Err(Error::new(format!("unknown 1.21.11 slot display {id}"))),
    })
}

pub fn decode_recipe_book(id: i32, payload: &[u8], target: BlockStateTarget) -> Result<Clientbound> {
    let mut r = Reader::new(payload);
    let slot = |r: &mut Reader| slot(r, target, 0);
    let packet = match id {
        clientbound::RECIPE_BOOK_ADD => {
            let count = r.count(r.remaining())?;
            let mut entries = Vec::with_capacity(count.min(4096));
            for _ in 0..count {
                let mut entry = RecipeBookEntry::read_with(&mut r, &slot)?;
                for set in entry.requirements.iter_mut().flatten() {
                    if let HolderSet::Items(ids) = set {
                        for id in ids {
                            *id = item(*id, target)?;
                        }
                    }
                }
                entries.push(entry);
            }
            Clientbound::RecipeBookAdd(RecipeBookAdd {
                entries,
                replace: r.bool()?,
            })
        }
        clientbound::PLACE_GHOST_RECIPE => Clientbound::PlaceGhostRecipe(PlaceGhostRecipe {
            container: r.varint()?,
            display: RecipeDisplay::read_with(&mut r, &slot)?,
        }),
        _ => return Err(Error::new("unknown 1.21.11 recipe packet")),
    };
    if r.remaining() != 0 {
        return Err(Error::new("trailing data in 1.21.11 recipe packet"));
    }
    Ok(packet)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_slot_displays_keep_stack_count_and_item_registry() {
        // Container 0: shapeless recipe, a planks tag input, and four of item 1.
        let mut w = crate::buf::Writer::new();
        w.bytes(&[0, 0, 1, 4]);
        w.string("minecraft:planks");
        w.bytes(&[3, 4, 1, 0, 0, 2, 1]);
        for target in [BlockStateTarget::Native12111, BlockStateTarget::Engine26_3] {
            let packet = decode_recipe_book(clientbound::PLACE_GHOST_RECIPE, &w.data, target)
                .unwrap();
            let Clientbound::PlaceGhostRecipe(PlaceGhostRecipe {
                display: RecipeDisplay::Shapeless { ingredients, result, .. },
                ..
            }) = packet else {
                panic!("expected a shapeless ghost recipe");
            };
            assert_eq!(
                ingredients,
                [SlotDisplay::Tag(HolderSet::Tag("minecraft:planks".into()))]
            );
            assert!(matches!(result, SlotDisplay::ItemStack(s) if s.count == 4 && s.item == 1));
        }
    }
}
