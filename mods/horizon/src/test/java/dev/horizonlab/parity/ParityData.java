package dev.horizonlab.parity;

import java.lang.reflect.Field;
import java.util.*;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.Executor;
import net.minecraft.SharedConstants;
import net.minecraft.core.*;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.nbt.*;
import net.minecraft.resources.RegistryDataLoader;
import net.minecraft.resources.RegistryOps;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.RegistryLayer;
import net.minecraft.server.ReloadableServerRegistries;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.server.packs.PackType;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.packs.resources.MultiPackResourceManager;
import net.minecraft.server.packs.resources.ResourceManager;
import net.minecraft.tags.TagLoader;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.level.Level;

/**
 * Shared set-up for the interaction/inventory parity oracles: the vanilla data
 * pack loaded into registries (as the server does), an NBT registry ops, a
 * stand-in ServerLevel that only answers registryAccess, and canonical text
 * for item stacks and NBT so both sides print comparable lines.
 */
public final class ParityData {
    public static HolderLookup.Provider lookup;
    public static RegistryAccess access;
    public static RegistryOps<Tag> ops;
    public static ServerLevel level;
    /** The loot tables of the vanilla data pack. */
    public static ReloadableServerRegistries.Holder loot;

    public static void init() throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        var repo = ServerPacksSource.createVanillaTrustedRepository();
        repo.reload();
        repo.setSelected(List.of("vanilla"));
        var resources = new MultiPackResourceManager(PackType.SERVER_DATA, repo.openAllSelected());
        var layers = RegistryLayer.createRegistryAccess();
        var staticTags = TagLoader.loadTagsForExistingRegistries(resources, layers.getLayer(RegistryLayer.STATIC));
        var worldLayers = loadWorldRegistries(resources, layers, staticTags, false);
        var loaded = ReloadableServerRegistries.reload(worldLayers, staticTags, resources, Runnable::run).join();
        loot = new ReloadableServerRegistries.Holder(loaded.lookupWithUpdatedTags());
        lookup = loaded.lookupWithUpdatedTags();
        applyStaticData(staticTags, lookup);
        access = loaded.layers().compositeAccess();
        ops = RegistryOps.create(NbtOps.INSTANCE, lookup);
        var unsafeField = Class.forName("sun.misc.Unsafe").getDeclaredField("theUnsafe");
        unsafeField.setAccessible(true);
        Object unsafe = unsafeField.get(null);
        level = (ServerLevel) unsafe.getClass().getMethod("allocateInstance", Class.class).invoke(unsafe, ServerLevel.class);
        Field registryAccess = Level.class.getDeclaredField("registryAccess");
        registryAccess.setAccessible(true);
        long offset = (long) unsafe.getClass().getMethod("objectFieldOffset", Field.class).invoke(unsafe, registryAccess);
        unsafe.getClass().getMethod("putObject", Object.class, long.class, Object.class).invoke(unsafe, level, offset, access);
    }

    static LayeredRegistryAccess<RegistryLayer> loadWorldRegistries(
            ResourceManager resources, LayeredRegistryAccess<RegistryLayer> layers,
            List<Registry.PendingTags<?>> staticTags, boolean includeDimensions) throws ReflectiveOperationException {
        java.lang.reflect.Method load;
        try {
            load = RegistryDataLoader.class.getMethod(
                "load", ResourceManager.class, List.class, List.class, Executor.class);
        } catch (NoSuchMethodException absentOn12111) {
            return loadLegacyWorldRegistries(resources, layers, staticTags);
        }
        var layer = RegistryLayer.valueOf("WORLD");
        var context = TagLoader.buildUpdatedLookups(layers.getAccessForLoading(layer), staticTags);
        var worldRegistries = RegistryDataLoader.class.getField("WORLD_REGISTRIES").get(null);
        Executor direct = Runnable::run;
        var world = (RegistryAccess.Frozen) ((CompletableFuture<?>) load.invoke(
            null, resources, context, worldRegistries, direct)).join();
        if (!includeDimensions) return layers.replaceFrom(layer, world);
        var dimensionRegistries = RegistryDataLoader.class.getField("DIMENSION_REGISTRIES").get(null);
        var dimensionContext = java.util.stream.Stream.concat(context.stream(), world.listRegistries()).toList();
        var dimensions = (RegistryAccess.Frozen) ((CompletableFuture<?>) load.invoke(
            null, resources, dimensionContext, dimensionRegistries, direct)).join();
        return layers.replaceFrom(layer, world, dimensions);
    }

    private static LayeredRegistryAccess<RegistryLayer> loadLegacyWorldRegistries(
            ResourceManager resources, LayeredRegistryAccess<RegistryLayer> layers,
            List<Registry.PendingTags<?>> staticTags) throws ReflectiveOperationException {
        var load = RegistryDataLoader.class.getMethod("load", ResourceManager.class, List.class, List.class);
        var worldgen = RegistryLayer.valueOf("WORLDGEN");
        var context = TagLoader.buildUpdatedLookups(layers.getAccessForLoading(worldgen), staticTags);
        var worldgenRegistries = RegistryDataLoader.class.getField("WORLDGEN_REGISTRIES").get(null);
        var world = (RegistryAccess.Frozen) load.invoke(null, resources, context, worldgenRegistries);
        layers = layers.replaceFrom(worldgen, world);

        var dimensions = RegistryLayer.DIMENSIONS;
        context = TagLoader.buildUpdatedLookups(layers.getAccessForLoading(dimensions), staticTags);
        var dimensionRegistries = RegistryDataLoader.class.getField("DIMENSION_REGISTRIES").get(null);
        var dimensionData = (RegistryAccess.Frozen) load.invoke(null, resources, context, dimensionRegistries);
        return layers.replaceFrom(dimensions, dimensionData);
    }

    static void applyStaticData(List<Registry.PendingTags<?>> staticTags, HolderLookup.Provider lookup)
            throws ReflectiveOperationException {
        Object initializers;
        try {
            initializers = BuiltInRegistries.class.getField("DATA_COMPONENT_INITIALIZERS").get(null);
        } catch (NoSuchFieldException absentOn12111) {
            // 1.21.11 already has its default components bound at bootstrap.
            staticTags.forEach(Registry.PendingTags::apply);
            return;
        }
        var build = initializers.getClass().getMethod("build", HolderLookup.Provider.class);
        var components = (List<?>) build.invoke(initializers, lookup);
        var apply = Class.forName("net.minecraft.core.component.DataComponentInitializers$PendingComponents")
            .getMethod("apply");
        staticTags.forEach(Registry.PendingTags::apply);
        for (var component : components) {
            apply.invoke(component);
        }
    }

    /** "-" is the empty stack; otherwise ItemStack.CODEC over SNBT. */
    public static ItemStack stack(String snbt) throws Exception {
        if (snbt.equals("-")) return ItemStack.EMPTY;
        Tag tag = TagParser.parseCompoundFully(snbt);
        return ItemStack.CODEC.parse(ops, tag).getOrThrow();
    }

    public static String canon(ItemStack stack) {
        if (stack == null || stack.isEmpty()) return "-";
        Tag tag = ItemStack.CODEC.encodeStart(ops, stack).getOrThrow();
        return canon(tag);
    }

    /** Sorted-key, type-annotated text of an NBT tag. */
    public static String canon(Tag tag) {
        StringBuilder out = new StringBuilder();
        canon(tag, out);
        return out.toString();
    }

    private static void canon(Tag tag, StringBuilder out) {
        switch (tag) {
            case ByteTag t -> out.append(t.value()).append('b');
            case ShortTag t -> out.append(t.value()).append('s');
            case IntTag t -> out.append(t.value());
            case LongTag t -> out.append(t.value()).append('L');
            case FloatTag t -> out.append(t.value()).append('f');
            case DoubleTag t -> out.append(t.value()).append('d');
            case StringTag t -> out.append('"').append(t.value().replace("\\", "\\\\").replace("\"", "\\\"")).append('"');
            case ByteArrayTag t -> {
                out.append("[B;");
                byte[] a = t.getAsByteArray();
                for (int i = 0; i < a.length; i++) out.append(i > 0 ? "," : "").append(a[i]);
                out.append(']');
            }
            case IntArrayTag t -> {
                out.append("[I;");
                int[] a = t.getAsIntArray();
                for (int i = 0; i < a.length; i++) out.append(i > 0 ? "," : "").append(a[i]);
                out.append(']');
            }
            case LongArrayTag t -> {
                out.append("[L;");
                long[] a = t.getAsLongArray();
                for (int i = 0; i < a.length; i++) out.append(i > 0 ? "," : "").append(a[i]);
                out.append(']');
            }
            case ListTag t -> {
                out.append('[');
                for (int i = 0; i < t.size(); i++) {
                    if (i > 0) out.append(',');
                    canon(t.get(i), out);
                }
                out.append(']');
            }
            case CompoundTag t -> {
                out.append('{');
                boolean first = true;
                for (String key : new TreeSet<>(t.keySet())) {
                    if (!first) out.append(',');
                    first = false;
                    out.append(key).append(':');
                    canon(t.get(key), out);
                }
                out.append('}');
            }
            default -> throw new IllegalStateException(tag.toString());
        }
    }
}
