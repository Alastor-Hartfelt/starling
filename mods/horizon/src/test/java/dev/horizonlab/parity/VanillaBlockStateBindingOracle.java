package dev.horizonlab.parity;

import com.google.gson.*;
import com.mojang.serialization.JsonOps;
import java.nio.file.*;
import java.util.*;
import net.minecraft.client.renderer.block.dispatch.*;
import net.minecraft.client.renderer.block.dispatch.multipart.MultiPartModel;
import net.minecraft.resources.Identifier;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.StateDefinition;
import net.minecraft.util.StrictJsonParser;
import net.minecraft.world.level.block.state.BlockState;
import static dev.horizonlab.parity.VanillaCuboidModelOracle.map;

/** Native registry binding and multipart visual-equality selection, without a GPU. */
public final class VanillaBlockStateBindingOracle {
    private static java.util.function.Function<Identifier,StateDefinition<Block,BlockState>> mapper;
    private static final Gson JSON = new GsonBuilder().serializeNulls().create();
    private static Object field(Object value,String name) throws Exception {
        var f=value.getClass().getDeclaredField(name);f.setAccessible(true);return f.get(value);
    }
    private static Object bind(JsonObject row) throws Exception {
        var states=Objects.requireNonNull(mapper.apply(Identifier.parse(row.get("block").getAsString())));
        var definition=BlockStateModelDispatcher.CODEC.parse(JsonOps.INSTANCE,StrictJsonParser.parse(row.get("json").getAsString())).getOrThrow();
        var roots=definition.instantiate(states,()->"horizon:binding_probe");
        var indices=new IdentityHashMap<BlockStateModel.Unbaked,Integer>();
        definition.simpleModels().ifPresent(m->{int index=0;for(var value:m.models().values())indices.put(value,index++);});
        var selected=new ArrayList<Object>();
        for(var state:states.getPossibleStates()) {
            var root=roots.get(state);
            if(root==null) {selected.add(null);continue;}
            if(root instanceof BlockStateModel.SimpleCachedUnbakedRoot) {
                var index=indices.get(field(root,"contents"));if(index==null)throw new AssertionError("unrecognized simple root");
                selected.add(map("variant",index));
            } else if(root instanceof MultiPartModel.Unbaked) {
                var group=root.visualEqualityGroup(state);Object parts=null;
                for(var f:group.getClass().getDeclaredFields())if(it.unimi.dsi.fastutil.ints.IntList.class.isAssignableFrom(f.getType())) {f.setAccessible(true);parts=f.get(group);}
                if(parts==null)throw new AssertionError("unrecognized multipart group");
                selected.add(map("multipart",parts));
            } else throw new AssertionError("unrecognized root "+root.getClass());
        }
        return selected;
    }
    public static void main(String[] args) throws Exception {
        net.minecraft.SharedConstants.tryDetectVersion();net.minecraft.server.Bootstrap.bootStrap();mapper=VanillaBlockStateSchemasExporter.mapper();
        if(args[0].equals("export")){VanillaBlockStateSchemasExporter.export(Path.of(args[1]),mapper);return;}
        try(var input=Files.newBufferedReader(Path.of(args[0]));var output=Files.newBufferedWriter(Path.of(args[1]))) {
            for(String line;(line=input.readLine())!=null;) {
                var row=JsonParser.parseString(line).getAsJsonObject();Object result;
                try {result=map("name",row.get("name").getAsString(),"selected",bind(row));}
                catch(RuntimeException error) {result=map("name",row.get("name").getAsString(),"error",true);}
                output.write(JSON.toJson(result));output.newLine();
            }
        }
    }
}
