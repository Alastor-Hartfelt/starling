package dev.horizonlab.parity;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.Set;
import java.util.TreeSet;
import net.minecraft.SharedConstants;
import net.minecraft.commands.Commands;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.Registry;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.resources.FileToIdConverter;
import net.minecraft.resources.ResourceKey;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.RegistryLayer;
import net.minecraft.server.ReloadableServerRegistries;
import net.minecraft.server.ReloadableServerResources;
import net.minecraft.server.packs.PackType;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.packs.resources.MultiPackResourceManager;
import net.minecraft.server.permissions.LevelBasedPermissionSet;
import net.minecraft.tags.TagLoader;
import net.minecraft.world.flag.FeatureFlags;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.properties.Property;
import net.minecraft.world.timeline.Timeline;

/** Registry data used by native command suggestions. */
public final class VanillaCommandRegistriesExporter {
    static final String[] PLAYERS = {"Player", "Steve", "Alex"};
    /** ClientSuggestionProvider.getSelectedEntities: the crosshair entity's UUID. */
    static final String[] SELECTED = {"0b9c6a7e-3d1f-4a2b-9c8d-7e6f5a4b3c2d"};
    static final String[] TEAMS = {"red", "blue"};
    static final String[] OBJECTIVES = {"kills", "health", "obj"};
    static final String[] TRACKED = {"#total", "Steve", "fake"};


    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        var repo = ServerPacksSource.createVanillaTrustedRepository();
        repo.reload();
        repo.setSelected(List.of("vanilla"));
        try (var resources = new MultiPackResourceManager(PackType.SERVER_DATA, repo.openAllSelected())) {
            var layers = RegistryLayer.createRegistryAccess();
            var staticTags = TagLoader.loadTagsForExistingRegistries(resources, layers.getLayer(RegistryLayer.STATIC));
            var worldLayers = ParityData.loadWorldRegistries(resources, layers, staticTags, true);
            var loaded = ReloadableServerRegistries.reload(worldLayers, staticTags, resources, Runnable::run).join();
            var managers = ReloadableServerResources.loadResources(resources, worldLayers, staticTags,
                FeatureFlags.VANILLA_SET, Commands.CommandSelection.INTEGRATED,
                LevelBasedPermissionSet.GAMEMASTER, Runnable::run, Runnable::run).join();
            var lookup = loaded.lookupWithUpdatedTags();
            ParityData.applyStaticData(staticTags, lookup);
            exportRegistries(Path.of(args[1]), loaded.layers().compositeAccess(), lookup, repo, resources, managers);
        }
    }

    private static JsonArray exportClocks(JsonObject out, RegistryAccess access) throws Exception {
        var markers = new JsonObject();
        var clocks = new JsonArray();
        var clockKey = ResourceKey.createRegistryKey(net.minecraft.resources.Identifier.withDefaultNamespace("world_clock"));
        var registry = access.lookup(clockKey);
        if (registry.isPresent()) {
            var clockNames = new TreeSet<String>();
            registry.get().keySet().forEach(id -> clockNames.add(id.toString()));
            var byClock = new java.util.TreeMap<String, Set<String>>();
            clockNames.forEach(id -> { clocks.add(id); byClock.put(id, new TreeSet<>()); });
            // The world clock and marker APIs were introduced after 1.21.11.
            var markerType = Class.forName("net.minecraft.world.clock.ClockTimeMarker");
            var show = markerType.getMethod("showInCommands");
            var clock = markerType.getMethod("clock");
            var register = Timeline.class.getMethod("registerTimeMarkers", java.util.function.BiConsumer.class);
            java.util.function.BiConsumer<ResourceKey<?>, Object> add = (key, marker) -> {
                try {
                    if ((boolean) show.invoke(marker)) {
                        var holder = (Holder<?>) clock.invoke(marker);
                        byClock.get(holder.unwrapKey().orElseThrow().identifier().toString()).add(key.identifier().toString());
                    }
                } catch (ReflectiveOperationException error) {
                    throw new IllegalStateException(error);
                }
            };
            for (var timeline : access.lookupOrThrow(Registries.TIMELINE)) register.invoke(timeline, add);
            byClock.forEach((id, names) -> {
                var values = new JsonArray();
                names.forEach(values::add);
                markers.add(id, values);
            });
        }
        out.add("clock_time_markers", markers);
        return clocks;
    }

    static void exportRegistries(Path out, RegistryAccess access, HolderLookup.Provider lookup, net.minecraft.server.packs.repository.PackRepository repo, MultiPackResourceManager resources, net.minecraft.server.ReloadableServerResources managers) throws Exception {
        JsonObject o = new JsonObject();
        Set<String> learnable = new TreeSet<>();
        managers.getRecipeManager().getRecipes().stream().filter(h -> !h.value().isSpecial())
            .forEach(h -> learnable.add(h.id().identifier().toString()));
        JsonArray learnableArr = new JsonArray();
        learnable.forEach(learnableArr::add);
        o.add("learnable_recipes", learnableArr);
        JsonObject criteria = new JsonObject();
        managers.getAdvancements().getAllAdvancements().stream()
            .sorted(java.util.Comparator.comparing(a -> a.id().toString()))
            .forEach(a -> {
                JsonArray names = new JsonArray();
                new TreeSet<>(a.value().criteria().keySet()).forEach(names::add);
                criteria.add(a.id().toString(), names);
            });
        o.add("advancement_criteria", criteria);
        JsonArray blockEntities = new JsonArray();
        for (Block block : BuiltInRegistries.BLOCK) {
            if (FeatureFlags.VANILLA_SET.isSubsetOf(block.requiredFeatures()) && block.defaultBlockState().hasBlockEntity())
                blockEntities.add(BuiltInRegistries.BLOCK.getKey(block).toString());
        }
        o.add("block_entities", blockEntities);
        Set<String> persistent = new TreeSet<>();
        for (var entry : BuiltInRegistries.DATA_COMPONENT_TYPE.entrySet())
            if (!entry.getValue().isTransient()) persistent.add(entry.getKey().identifier().toString());
        JsonArray persistentArr = new JsonArray();
        persistent.forEach(persistentArr::add);
        o.add("persistent_components", persistentArr);
        JsonObject blockTagMembers = new JsonObject();
        lookup.lookupOrThrow(Registries.BLOCK).listTags().sorted(java.util.Comparator.comparing(t -> t.key().location().toString())).forEach(tag -> {
            JsonArray members = new JsonArray();
            tag.forEach(holder -> members.add(holder.unwrapKey().orElseThrow().identifier().toString()));
            blockTagMembers.add(tag.key().location().toString(), members);
        });
        o.add("block_tag_members", blockTagMembers);
        JsonArray selected = new JsonArray();
        repo.getSelectedIds().forEach(selected::add);
        o.add("datapacks_selected", selected);
        JsonArray available = new JsonArray();
        repo.getAvailableIds().forEach(available::add);
        o.add("datapacks_available", available);
        JsonArray enableable = new JsonArray();
        repo.getAvailablePacks().stream().filter(p -> p.getRequestedFeatures().isSubsetOf(FeatureFlags.VANILLA_SET))
            .forEach(p -> enableable.add(p.getId()));
        o.add("datapacks_enableable", enableable);
        var clocks = exportClocks(o, access);
        Set<String> templates = new TreeSet<>();
        var converter = new FileToIdConverter("structure", ".nbt");
        converter.listMatchingResources(resources).keySet().forEach(file -> templates.add(converter.fileToId(file).toString()));
        JsonArray templateArr = new JsonArray();
        templates.forEach(templateArr::add);
        o.add("templates", templateArr);
        JsonArray items = new JsonArray();
        for (var item : BuiltInRegistries.ITEM) {
            if (FeatureFlags.VANILLA_SET.isSubsetOf(item.requiredFeatures()))
                items.add(BuiltInRegistries.ITEM.getKey(item).toString());
        }
        o.add("items", items);
        JsonObject blocks = new JsonObject();
        for (Block block : BuiltInRegistries.BLOCK) {
            if (!FeatureFlags.VANILLA_SET.isSubsetOf(block.requiredFeatures())) continue;
            JsonArray props = new JsonArray();
            for (Property<?> p : block.getStateDefinition().getProperties()) {
                JsonArray values = new JsonArray();
                for (Object v : p.getPossibleValues()) values.add(((Property) p).getName((Comparable) v));
                JsonArray pair = new JsonArray();
                pair.add(p.getName());
                pair.add(values);
                props.add(pair);
            }
            blocks.add(BuiltInRegistries.BLOCK.getKey(block).toString(), props);
        }
        o.add("blocks", blocks);
        o.add("item_tags", tags(lookup, Registries.ITEM));
        o.add("block_tags", tags(lookup, Registries.BLOCK));
        JsonArray summonable = new JsonArray();
        JsonArray types = new JsonArray();
        for (var type : BuiltInRegistries.ENTITY_TYPE) {
            if (!FeatureFlags.VANILLA_SET.isSubsetOf(type.requiredFeatures())) continue;
            String id = BuiltInRegistries.ENTITY_TYPE.getKey(type).toString();
            types.add(id);
            if (type.canSummon()) summonable.add(id);
        }
        o.add("entity_types", types);
        o.add("summonable", summonable);
        o.add("effects", ids(BuiltInRegistries.MOB_EFFECT));
        o.add("attributes", ids(BuiltInRegistries.ATTRIBUTE));
        o.add("sounds", ids(BuiltInRegistries.SOUND_EVENT));
        o.add("enchantments", dynamic(access, Registries.ENCHANTMENT));
        o.add("structures", dynamic(access, Registries.STRUCTURE));
        o.add("biomes", dynamic(access, Registries.BIOME));
        o.add("damage_types", dynamic(access, Registries.DAMAGE_TYPE));
        o.add("clocks", clocks);
        o.add("timelines", dynamic(access, Registries.TIMELINE));
        o.add("dimensions", arr("minecraft:overworld", "minecraft:the_nether", "minecraft:the_end"));
        o.add("players", arr(PLAYERS));
        o.add("selected_entities", arr(SELECTED));
        o.add("teams", arr(TEAMS));
        o.add("objectives", arr(OBJECTIVES));
        o.add("tracked", arr(TRACKED));
        JsonObject by = new JsonObject();
        lookup.listRegistryKeys().sorted(java.util.Comparator.comparing(k -> k.identifier().toString())).forEach(key -> {
            var reg = lookup.lookupOrThrow(key);
            JsonObject entry = new JsonObject();
            java.util.List<String> ids = reg.listElementIds().map(k -> k.identifier().toString()).sorted().toList();
            JsonArray idArr = new JsonArray();
            ids.forEach(idArr::add);
            entry.add("ids", idArr);
            JsonArray tagArr = new JsonArray();
            reg.listTagIds().map(t -> t.location().toString()).sorted().forEach(tagArr::add);
            entry.add("tags", tagArr);
            by.add(key.identifier().toString(), entry);
        });
        o.add("by_registry", by);
        Files.writeString(out.resolve("registries.json"), o.toString());
    }

    static JsonArray arr(String... values) {
        JsonArray a = new JsonArray();
        for (String v : values) a.add(v);
        return a;
    }

    static <T> JsonArray ids(Registry<T> registry) {
        Set<String> sorted = new TreeSet<>();
        for (var key : registry.keySet()) sorted.add(key.toString());
        JsonArray a = new JsonArray();
        sorted.forEach(a::add);
        return a;
    }

    static <T> JsonArray dynamic(RegistryAccess access, ResourceKey<? extends Registry<T>> key) {
        return ids(access.lookupOrThrow(key));
    }

    static <T> JsonArray tags(HolderLookup.Provider lookup, ResourceKey<? extends Registry<T>> key) {
        Set<String> sorted = new TreeSet<>();
        lookup.lookupOrThrow(key).listTagIds().forEach(t -> sorted.add(t.location().toString()));
        JsonArray a = new JsonArray();
        sorted.forEach(a::add);
        return a;
    }
}
