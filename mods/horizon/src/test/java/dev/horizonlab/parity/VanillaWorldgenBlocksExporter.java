package dev.horizonlab.parity;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import it.unimi.dsi.fastutil.objects.Object2IntMap;
import java.lang.reflect.Modifier;
import java.nio.file.Files;
import java.nio.file.Path;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.tags.TagKey;
import net.minecraft.world.level.BlockGetter;
import net.minecraft.world.level.LevelReader;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.state.BlockState;

/** Definition inputs used by the Rust world generator. */
public final class VanillaWorldgenBlocksExporter {
    private static Class<?> declaring(Class<?> type, String method, Class<?>... parameters) {
        while (type != null) {
            try {
                return type.getDeclaredMethod(method, parameters).getDeclaringClass();
            } catch (NoSuchMethodException absent) {
                type = type.getSuperclass();
            }
        }
        return null;
    }

    static void survival(Path dir) throws Exception {
        var owners = new JsonObject();
        var placement = new JsonObject();
        var tags = new JsonObject();
        var gates = new JsonArray();
        var ignition = new JsonObject();
        var field = FireBlock.class.getDeclaredField("igniteOdds");
        field.setAccessible(true);
        var odds = (Object2IntMap<?>) field.get(Blocks.FIRE);
        for (var block : BuiltInRegistries.BLOCK) {
            String name = BuiltInRegistries.BLOCK.getKey(block).toString();
            if (block instanceof FenceGateBlock) gates.add(name);
            ignition.addProperty(name, odds.getInt(block));
            owners.addProperty(name, declaring(block.getClass(), "canSurvive",
                BlockState.class, LevelReader.class, BlockPos.class).getSimpleName());
            var owner = declaring(block.getClass(), "mayPlaceOn",
                BlockState.class, BlockGetter.class, BlockPos.class);
            if (owner != null) placement.addProperty(name, owner.getSimpleName());
            var fields = new JsonObject();
            for (Class<?> c = block.getClass(); c != null; c = c.getSuperclass()) {
                for (var f : c.getDeclaredFields()) {
                    if (f.getType() != TagKey.class || Modifier.isStatic(f.getModifiers())) continue;
                    f.setAccessible(true);
                    var tag = (TagKey<?>) f.get(block);
                    if (tag != null) fields.addProperty(f.getName(), tag.location().toString());
                }
            }
            if (!fields.isEmpty()) tags.add(name, fields);
        }
        var extra = new JsonObject();
        extra.add("fence_gates", gates);
        extra.add("ignition_odds", ignition);
        Files.writeString(dir.resolve("survival-extra-definitions.json"), extra.toString());
        Files.writeString(dir.resolve("survival-classes.json"), owners.toString());
        Files.writeString(dir.resolve("survival-placement-classes.json"), placement.toString());
        Files.writeString(dir.resolve("survival-support-tags.json"), tags.toString());
    }

    static void placement(Path dir) throws Exception {
        var doubles = new JsonArray();
        var moss = new JsonArray();
        for (var block : BuiltInRegistries.BLOCK) {
            String name = BuiltInRegistries.BLOCK.getKey(block).toString();
            if (block instanceof DoublePlantBlock) doubles.add(name);
            if (block instanceof MossyCarpetBlock) moss.add(name);
        }
        var data = new JsonObject();
        data.add("double_plants", doubles);
        data.add("mossy_carpets", moss);
        Files.writeString(dir.resolve("placement-definitions.json"), data.toString());
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        VanillaBlockPropertiesOracle.main(args);
        VanillaBlockSupportOracle.main(args);
        var dir = Path.of(args[1]);
        survival(dir);
        placement(dir);
    }
}
