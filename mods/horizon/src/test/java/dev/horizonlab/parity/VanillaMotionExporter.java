package dev.horizonlab.parity;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import java.io.DataOutputStream;
import java.lang.reflect.Field;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Set;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.nbt.CompoundTag;
import net.minecraft.nbt.ListTag;
import net.minecraft.nbt.NbtIo;
import net.minecraft.server.Bootstrap;
import net.minecraft.sounds.SoundEvents;
import net.minecraft.world.entity.Entity;
import net.minecraft.world.level.Level;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.state.BlockBehaviour;
import net.minecraft.world.level.block.state.BlockState;

/** Movement and landing inputs for the native client. */
public final class VanillaMotionExporter {
    private static Field field(Class<?> owner, String name) throws ReflectiveOperationException {
        var field = owner.getDeclaredField(name);
        field.setAccessible(true);
        return field;
    }

    private static int dataVersion() {
        return SharedConstants.getCurrentVersion().dataVersion().version();
    }

    static void movement(Path dir) throws Exception {
        boolean legacy = dataVersion() == 4671;
        String[] names = legacy ? new String[]{"friction", "speedFactor", "jumpFactor"}
            : new String[]{"friction", "speedFactor", "jumpFactor", "bounceRestitution", "fallDistanceReduction"};
        Field[] fields = new Field[names.length];
        for (int i = 0; i < names.length; i++) fields[i] = field(BlockBehaviour.class, names[i]);

        var states = new ListTag();
        var defaults = new CompoundTag();
        for (var state : Block.BLOCK_STATE_REGISTRY) {
            states.add(VanillaCollisionOracle.encode(state));
            if (state == state.getBlock().defaultBlockState()) {
                defaults.putInt(BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString(), Block.getId(state));
            }
        }
        var definitions = new CompoundTag();
        var ids = new JsonObject();
        for (var block : BuiltInRegistries.BLOCK) {
            var data = new CompoundTag();
            for (int i = 0; i < fields.length; i++) data.putFloat(names[i], fields[i].getFloat(block));
            if (legacy) {
                // In 1.21.11 these coefficients live in the bed and slime movement callbacks.
                float bounce = block instanceof BedBlock ? 0.66f : block instanceof SlimeBlock ? 1f : 0f;
                data.putFloat("bounceRestitution", bounce);
                data.putFloat("fallDistanceReduction", block instanceof BedBlock ? 0.5f : 0f);
            }
            data.putBoolean("fenceGate", block instanceof FenceGateBlock);
            String name = BuiltInRegistries.BLOCK.getKey(block).toString();
            definitions.put(name, data);
            ids.addProperty(name, Block.getId(block.defaultBlockState()));
        }
        var info = new JsonObject();
        info.addProperty("states", states.size());
        info.add("defaults", ids);
        Files.writeString(dir.resolve("registry-info.json"), info.toString());
        var registry = new CompoundTag();
        registry.putInt("DataVersion", dataVersion());
        registry.put("states", states);
        registry.put("defaults", defaults);
        write(dir.resolve("block-registry.nbt"), registry);
        var movement = new CompoundTag();
        movement.putInt("DataVersion", dataVersion());
        movement.put("states", states);
        movement.put("blocks", definitions);
        write(dir.resolve("movement-properties.nbt"), movement);
    }

    private static void write(Path file, CompoundTag value) throws Exception {
        try (var out = new DataOutputStream(Files.newOutputStream(file))) {
            NbtIo.write(value, out);
        }
    }

    static void landing(Path dir) throws Exception {
        boolean legacy = dataVersion() == 4671;
        var sound = field(BlockBehaviour.class, "soundType");
        var definitions = new JsonObject();
        var owners = Set.of("Block", "HayBlock", "HoneyBlock", "SlimeBlock", "PowderSnowBlock",
            "PointedDripstoneBlock", "FarmlandBlock", "TurtleEggBlock");
        for (var block : BuiltInRegistries.BLOCK) {
            var owner = block.getClass().getMethod("fallOn", Level.class, BlockState.class,
                BlockPos.class, Entity.class, double.class).getDeclaringClass();
            String family = owner.getSimpleName();
            // BedBlock delegates to the default fall handler with half the distance.
            if (legacy && owner == BedBlock.class) family = "Block";
            if (legacy && family.equals("FarmBlock")) family = "FarmlandBlock";
            if (!owners.contains(family)) throw new IllegalStateException("Unknown landing family " + owner);
            var entry = new JsonObject();
            entry.addProperty("fall", family);
            boolean shelf = false;
            if (!legacy) {
                var bounce = block.getClass().getMethod("bounceOn", Level.class, BlockState.class,
                    BlockPos.class, Entity.class, double.class).getDeclaringClass();
                shelf = bounce.getSimpleName().equals("ShelfMushroomBlock");
                if (!shelf && bounce != Block.class) throw new IllegalStateException("Unknown bounce family " + bounce);
            }
            entry.addProperty("bounce", shelf);
            if (family.equals("FarmlandBlock")) {
                var base = legacy ? Blocks.DIRT : (Block) field(owner, "baseBlock").get(block);
                entry.addProperty("base", BuiltInRegistries.BLOCK.getKey(base).toString());
            }
            if (owner == HoneyBlock.class) {
                var type = (SoundType) sound.get(block);
                if (type.getFallSound() != SoundEvents.HONEY_BLOCK_FALL) throw new IllegalStateException("Honey sound");
                entry.addProperty("volume", String.format("%08x", Float.floatToIntBits(type.getVolume())));
                entry.addProperty("pitch", String.format("%08x", Float.floatToIntBits(type.getPitch())));
            }
            definitions.add(BuiltInRegistries.BLOCK.getKey(block).toString(), entry);
        }
        var metadata = new JsonArray();
        for (var state : Block.BLOCK_STATE_REGISTRY) {
            var row = new JsonObject();
            row.addProperty("id", Block.getId(state));
            row.addProperty("name", BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString());
            metadata.add(row);
        }
        Files.writeString(dir.resolve("landing-definitions.json"), definitions.toString());
        Files.writeString(dir.resolve("landing-states.json"), metadata.toString());
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        var dir = Path.of(args[1]);
        VanillaCollisionOracle.main(new String[]{"generate", args[1]});
        movement(dir);
        landing(dir);
    }
}
