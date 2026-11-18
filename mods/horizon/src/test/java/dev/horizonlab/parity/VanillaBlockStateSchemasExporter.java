package dev.horizonlab.parity;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.*;
import java.util.function.Function;
import net.minecraft.SharedConstants;
import net.minecraft.client.resources.model.BlockStateDefinitions;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.resources.Identifier;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.StateDefinition;
import net.minecraft.world.level.block.state.properties.IntegerProperty;
import net.minecraft.world.level.block.state.properties.Property;

/** Export the state schema consumed by Radio's blockstate loader. */
public final class VanillaBlockStateSchemasExporter {
    private static final Gson JSON = new GsonBuilder().serializeNulls().create();

    @SuppressWarnings("unchecked")
    static Function<Identifier, StateDefinition<Block, BlockState>> mapper() throws ReflectiveOperationException {
        var method = BlockStateDefinitions.class.getDeclaredMethod("definitionLocationToBlockStateMapper");
        // The mapper exists in both versions and has package access in 1.21.11.
        method.setAccessible(true);
        return (Function<Identifier, StateDefinition<Block, BlockState>>) method.invoke(null);
    }

    static void export(Path path, Function<Identifier, StateDefinition<Block, BlockState>> mapper)
            throws Exception {
        var blocks = new TreeMap<String, Object>();
        var ids = new TreeSet<>(BuiltInRegistries.BLOCK.keySet());
        ids.add(Identifier.parse("minecraft:item_frame"));
        ids.add(Identifier.parse("minecraft:glow_item_frame"));
        for (var id : ids) {
            var definition = Objects.requireNonNull(mapper.apply(id), id.toString());
            var schema = new TreeMap<String, Object>();
            for (var property : definition.getProperties()) {
                if (property instanceof IntegerProperty integer) {
                    var values = integer.getPossibleValues();
                    schema.put(property.getName(), Map.of(
                        "integer", new int[]{Collections.min(values), Collections.max(values)}
                    ));
                } else {
                    schema.put(property.getName(), Map.of("named", values(property)));
                }
            }

            var states = new ArrayList<Map<String, String>>();
            for (var state : definition.getPossibleStates()) {
                var properties = new TreeMap<String, String>();
                for (var property : definition.getProperties()) {
                    properties.put(property.getName(), value(state, property));
                }
                states.add(properties);
            }
            blocks.put(id.toString(), new TreeMap<>(Map.of("schema", schema, "states", states)));
        }
        Files.writeString(path, JSON.toJson(blocks));
    }

    private static <T extends Comparable<T>> Set<String> values(Property<T> property) {
        var values = new TreeSet<String>();
        for (var value : property.getPossibleValues()) {
            values.add(property.getName(value));
        }
        return values;
    }

    private static <T extends Comparable<T>> String value(BlockState state, Property<T> property) {
        return property.getName(state.getValue(property));
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        export(Path.of(args[0]), mapper());
    }
}
