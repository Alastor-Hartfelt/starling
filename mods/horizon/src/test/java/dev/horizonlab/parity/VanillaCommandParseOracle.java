package dev.horizonlab.parity;

import static dev.horizonlab.parity.VanillaCommandRegistriesExporter.*;
import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import com.mojang.brigadier.CommandDispatcher;
import com.mojang.brigadier.ParseResults;
import com.mojang.brigadier.StringReader;
import com.mojang.brigadier.exceptions.CommandSyntaxException;
import com.mojang.brigadier.context.CommandContextBuilder;
import com.mojang.brigadier.context.ParsedCommandNode;
import com.mojang.brigadier.context.ContextChain;
import com.mojang.brigadier.suggestion.Suggestion;
import com.mojang.brigadier.suggestion.Suggestions;
import java.lang.reflect.Field;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Collection;
import java.util.List;
import java.util.Set;
import net.minecraft.SharedConstants;
import net.minecraft.commands.CommandBuildContext;
import net.minecraft.commands.CommandSource;
import net.minecraft.commands.CommandSourceStack;
import net.minecraft.commands.Commands;
import net.minecraft.commands.SharedSuggestionProvider;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.Registry;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.registries.Registries;
import net.minecraft.network.chat.Component;
import net.minecraft.resources.ResourceKey;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.MinecraftServer;
import net.minecraft.server.RegistryLayer;
import net.minecraft.server.ReloadableServerRegistries;
import net.minecraft.server.ServerScoreboard;
import net.minecraft.server.dedicated.DedicatedServer;
import net.minecraft.server.packs.PackType;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.packs.resources.MultiPackResourceManager;
import net.minecraft.server.permissions.LevelBasedPermissionSet;
import net.minecraft.server.permissions.PermissionLevel;
import net.minecraft.server.permissions.PermissionSet;
import net.minecraft.server.players.PlayerList;
import net.minecraft.tags.TagLoader;
import net.minecraft.world.flag.FeatureFlagSet;
import net.minecraft.world.flag.FeatureFlags;
import net.minecraft.world.level.Level;
import net.minecraft.world.phys.Vec2;
import net.minecraft.world.phys.Vec3;

/**
 * Runs a corpus of command strings through vanilla's real Brigadier command
 * tree and prints, per input, whether it parses, the error (message and
 * cursor), the parsed nodes and the completion suggestions at the end of the
 * input. Also exports the registry contents the Rust side needs.
 *
 * usage: registries|tree|run OUT_DIR [CORPUS PERMISSION_LEVEL | SELECTION]
 */
public final class VanillaCommandParseOracle {
    static sun.misc.Unsafe unsafe;
    static final class Src extends CommandSourceStack {
        final RegistryAccess access;
        final HolderLookup.Provider lookup;
        Src(MinecraftServer server, PermissionSet perms, RegistryAccess access, HolderLookup.Provider lookup) {
            this(server, null, perms, access, lookup);
        }
        Src(MinecraftServer server, net.minecraft.server.level.ServerLevel level, PermissionSet perms, RegistryAccess access, HolderLookup.Provider lookup) {
            super(CommandSource.NULL, Vec3.ZERO, Vec2.ZERO, level, perms, Component.literal("Player"), server);
            this.access = access;
            this.lookup = lookup;
        }
        @Override public Collection<String> getOnlinePlayerNames() { return new ArrayList<>(List.of(PLAYERS)); }
        @Override public Collection<String> getSelectedEntities() { return new ArrayList<>(List.of(SELECTED)); }
        @Override public Collection<String> getAllTeams() { return new ArrayList<>(List.of(TEAMS)); }
        @Override public RegistryAccess registryAccess() { return access; }
        @Override public Set<ResourceKey<Level>> levels() { return Set.of(Level.OVERWORLD, Level.NETHER, Level.END); }
        @Override public FeatureFlagSet enabledFeatures() { return FeatureFlags.VANILLA_SET; }
        @Override public <E> java.util.concurrent.CompletableFuture<Suggestions> suggestRegistryElements(ResourceKey<? extends Registry<E>> key, SharedSuggestionProvider.ElementSuggestionType elements, com.mojang.brigadier.suggestion.SuggestionsBuilder builder, com.mojang.brigadier.context.CommandContext<?> context, java.util.function.Predicate<E> filter) {
            java.util.Optional<? extends HolderLookup<E>> registry = access.lookup(key);
            if (registry.isEmpty()) registry = lookup.lookup(key);
            if (registry.isEmpty()) return Suggestions.empty();
            this.suggestRegistryElements(registry.get(), elements, builder, filter);
            return builder.buildFuture();
        }
    }

    static void put(Object target, Class<?> owner, String name, Object value) throws Exception {
        Field f = owner.getDeclaredField(name);
        long offset = unsafe.objectFieldOffset(f);
        unsafe.putObject(target, offset, value);
    }

    static String message(CommandSyntaxException e) {
        com.mojang.brigadier.Message m = e.getRawMessage();
        return m.getString();
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        var unsafeField = sun.misc.Unsafe.class.getDeclaredField("theUnsafe");
        unsafeField.setAccessible(true);
        unsafe = (sun.misc.Unsafe) unsafeField.get(null);
        Path out = Path.of(args[1]);
        var repo = ServerPacksSource.createVanillaTrustedRepository();
        repo.reload();
        repo.setSelected(List.of("vanilla"));
        var resources = new MultiPackResourceManager(PackType.SERVER_DATA, repo.openAllSelected());
        var layers = RegistryLayer.createRegistryAccess();
        var staticTags = TagLoader.loadTagsForExistingRegistries(resources, layers.getLayer(RegistryLayer.STATIC));
        var worldLayers = ParityData.loadWorldRegistries(resources, layers, staticTags, true);
        var loaded = ReloadableServerRegistries.reload(worldLayers, staticTags, resources, Runnable::run).join();
        // The data pack managers (advancements, functions, recipes) a running server holds.
        var managers = net.minecraft.server.ReloadableServerResources.loadResources(
            resources, worldLayers, staticTags, FeatureFlags.VANILLA_SET, Commands.CommandSelection.INTEGRATED,
            LevelBasedPermissionSet.GAMEMASTER, Runnable::run, Runnable::run).join();
        HolderLookup.Provider lookup = loaded.lookupWithUpdatedTags();
        ParityData.applyStaticData(staticTags, lookup);
        RegistryAccess access = loaded.layers().compositeAccess();

        if (args[0].equals("registries")) {
            exportRegistries(out, access, lookup, repo, resources, managers);
            return;
        }
        FeatureFlagSet features = FeatureFlags.VANILLA_SET;
        CommandBuildContext buildContext = CommandBuildContext.simple(lookup, features);
        // `tree OUT_DIR ALL` exports the tree of every command (dedicated and integrated).
        var selection = args[0].equals("tree") && args.length > 2 ? Commands.CommandSelection.valueOf(args[2]) : Commands.CommandSelection.INTEGRATED;
        Commands commands = new Commands(selection, buildContext);
        CommandDispatcher<CommandSourceStack> dispatcher = commands.getDispatcher();

        if (args[0].equals("tree")) {
            Files.writeString(out.resolve("tree.json"), net.minecraft.commands.synchronization.ArgumentUtils.serializeNodeToJson(dispatcher, dispatcher.getRoot()).toString());
            return;
        }
        MinecraftServer server = (MinecraftServer) unsafe.allocateInstance(DedicatedServer.class);
        // The minimal state of a freshly created world that suggestion providers read:
        // empty command storage, stopwatches, boss bars, random sequences and
        // scheduled functions, the world clocks, the vanilla data pack (repository,
        // advancements, functions, structure templates) and an overworld level.
        put(server, MinecraftServer.class, "registries", loaded.layers());
        Class<?> reloadable = Class.forName("net.minecraft.server.MinecraftServer$ReloadableResources");
        var reloadableCtor = reloadable.getDeclaredConstructor(net.minecraft.server.packs.resources.CloseableResourceManager.class, net.minecraft.server.ReloadableServerResources.class);
        reloadableCtor.setAccessible(true);
        put(server, MinecraftServer.class, "resources", reloadableCtor.newInstance(resources, managers));
        put(server, MinecraftServer.class, "functionManager", new net.minecraft.server.ServerFunctionManager(server, managers.getFunctionLibrary()));
        put(server, MinecraftServer.class, "packRepository", repo);
        put(server, MinecraftServer.class, "commandStorage", new net.minecraft.world.level.storage.CommandStorage(null));
        var stopwatchesCtor = net.minecraft.world.Stopwatches.class.getDeclaredConstructor();
        stopwatchesCtor.setAccessible(true);
        put(server, MinecraftServer.class, "stopwatches", stopwatchesCtor.newInstance());
        put(server, MinecraftServer.class, "customBossEvents", new net.minecraft.server.bossevents.CustomBossEvents());
        put(server, MinecraftServer.class, "randomSequences", new net.minecraft.world.RandomSequences());
        put(server, MinecraftServer.class, "scheduledEvents", new net.minecraft.world.level.timers.TimerQueue<MinecraftServer>());
        var clocks = net.minecraft.world.clock.ServerClockManager.TYPE.constructor().get();
        clocks.init(server);
        put(server, MinecraftServer.class, "clockManager", clocks);
        var templates = (net.minecraft.world.level.levelgen.structure.templatesystem.StructureTemplateManager) unsafe.allocateInstance(
            net.minecraft.world.level.levelgen.structure.templatesystem.StructureTemplateManager.class);
        put(templates, net.minecraft.world.level.levelgen.structure.templatesystem.StructureTemplateManager.class, "sources", List.of(templateSource(lookup, resources)));
        put(server, MinecraftServer.class, "structureTemplateManager", templates);
        var level = (net.minecraft.server.level.ServerLevel) unsafe.allocateInstance(net.minecraft.server.level.ServerLevel.class);
        put(level, Level.class, "dimensionTypeRegistration",
            access.lookupOrThrow(Registries.DIMENSION_TYPE).getOrThrow(net.minecraft.world.level.dimension.BuiltinDimensionTypes.OVERWORLD));
        put(level, net.minecraft.server.level.ServerLevel.class, "server", server);
        PlayerList playerList = (PlayerList) unsafe.allocateInstance(net.minecraft.server.dedicated.DedicatedPlayerList.class);
        put(playerList, PlayerList.class, "players", new ArrayList<>());
        put(server, MinecraftServer.class, "playerList", playerList);
        ServerScoreboard scoreboard = new ServerScoreboard(server);
        put(server, MinecraftServer.class, "scoreboard", scoreboard);
        for (String team : TEAMS) scoreboard.addPlayerTeam(team);
        for (String objective : OBJECTIVES)
            scoreboard.addObjective(objective, net.minecraft.world.scores.criteria.ObjectiveCriteria.DUMMY, Component.literal(objective),
                net.minecraft.world.scores.criteria.ObjectiveCriteria.RenderType.INTEGER, true, null);
        Object holder = null;
        for (String tracked : TRACKED)
            scoreboard.getOrCreatePlayerScore(net.minecraft.world.scores.ScoreHolder.forNameOnly(tracked), scoreboard.getObjective("kills"));
        PermissionSet perms = LevelBasedPermissionSet.forLevel(PermissionLevel.byId(Integer.parseInt(args[3])));
        Src source = new Src(server, level, perms, access, lookup);

        List<String> lines = Files.readAllLines(Path.of(args[2]), StandardCharsets.UTF_8);
        java.io.BufferedWriter sb = Files.newBufferedWriter(out.resolve("vanilla.jsonl"));
        for (String raw : lines) {
            String input = raw;
            JsonObject row = new JsonObject();
            row.addProperty("in", input);
            try {
                ParseResults<CommandSourceStack> parse = dispatcher.parse(new StringReader(input), source);
                JsonObject result = new JsonObject();
                String error = null;
                int cursor = 0;
                if (parse.getReader().canRead()) {
                    if (parse.getExceptions().size() == 1) {
                        CommandSyntaxException e = parse.getExceptions().values().iterator().next();
                        error = message(e);
                        cursor = e.getCursor();
                    } else if (parse.getContext().getRange().isEmpty()) {
                        error = Component.translatable("command.unknown.command").getString();
                        cursor = parse.getReader().getCursor();
                    } else {
                        error = Component.translatable("command.unknown.argument").getString();
                        cursor = parse.getReader().getCursor();
                    }
                } else {
                    var ctx = parse.getContext().build(input);
                    if (ContextChain.tryFlatten(ctx).isEmpty()) {
                        error = Component.translatable("command.unknown.command").getString();
                        cursor = parse.getReader().getCursor();
                    }
                }
                if (error == null) {
                    result.addProperty("ok", true);
                    JsonArray nodes = new JsonArray();
                    CommandContextBuilder<CommandSourceStack> b = parse.getContext();
                    while (b != null) {
                        for (ParsedCommandNode<CommandSourceStack> n : b.getNodes()) nodes.add(n.getNode().getName());
                        b = b.getChild();
                    }
                    result.add("nodes", nodes);
                } else {
                    result.addProperty("ok", false);
                    result.addProperty("error", error);
                    result.addProperty("cursor", cursor);
                }
                row.add("parse", result);
                Suggestions suggestions = dispatcher.getCompletionSuggestions(parse, input.length()).get();
                JsonObject sg = new JsonObject();
                sg.addProperty("start", suggestions.getRange().getStart());
                sg.addProperty("end", suggestions.getRange().getEnd());
                JsonArray list = new JsonArray();
                for (Suggestion s : suggestions.getList()) {
                    String text = s.getText();
                    if (s.getTooltip() != null) text += "\t" + s.getTooltip().getString();
                    list.add(text);
                }
                sg.add("list", list);
                row.add("sugg", sg);
            } catch (Throwable t) {
                Throwable root = t;
                while (root.getCause() != null) root = root.getCause();
                StackTraceElement[] st = root.getStackTrace();
                row.addProperty("crash", root + (st.length > 0 ? " @ " + st[0] : ""));
            }
            sb.write(row.toString());
            sb.write("\n");
        }
        sb.close();
    }

    static net.minecraft.world.level.levelgen.structure.templatesystem.loader.ResourceManagerTemplateSource templateSource(HolderLookup.Provider lookup, MultiPackResourceManager resources) {
        return new net.minecraft.world.level.levelgen.structure.templatesystem.loader.ResourceManagerTemplateSource(
            net.minecraft.util.datafix.DataFixers.getDataFixer(), lookup.lookupOrThrow(Registries.BLOCK), resources,
            new net.minecraft.resources.FileToIdConverter("structure", ".nbt"));
    }

}
