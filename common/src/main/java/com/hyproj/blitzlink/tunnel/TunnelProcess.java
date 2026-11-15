package com.hyproj.blitzlink.tunnel;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
import java.util.function.Consumer;

import com.google.gson.Gson;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import com.hyproj.blitzlink.BlitzLink;

/**
 * One running {@code blitzlink-tunnel} (host or join). Control is one JSON object per line, tagged by {@code t}
 * (docs/PROTOCOL.md): we write to its stdin, it writes events to stdout; stderr goes to the log.
 * Events are delivered on the reader thread — callers hop to the game thread themselves.
 */
public final class TunnelProcess implements AutoCloseable {
    private static final Gson GSON = new Gson();
    private static final Set<TunnelProcess> LIVE = ConcurrentHashMap.newKeySet();

    static {
        Runtime.getRuntime().addShutdownHook(new Thread(() -> LIVE.forEach(TunnelProcess::close), "blitzlink-tunnel-reaper"));
    }

    private final Process process;
    private final OutputStream stdin;
    private final String label;
    private volatile boolean closed;

    private TunnelProcess(Process process, String label, Consumer<JsonObject> onEvent, Runnable onExit) {
        this.process = process;
        this.stdin = process.getOutputStream();
        this.label = label;
        LIVE.add(this);
        Thread out = new Thread(() -> {
            try (BufferedReader r = new BufferedReader(new InputStreamReader(process.getInputStream(), StandardCharsets.UTF_8))) {
                String line;
                while ((line = r.readLine()) != null) {
                    if (line.isBlank()) continue;
                    try {
                        onEvent.accept(JsonParser.parseString(line).getAsJsonObject());
                    } catch (RuntimeException e) {
                        BlitzLink.LOGGER.debug("[{}] {}", label, line);
                    }
                }
            } catch (IOException ignored) {
            } finally {
                LIVE.remove(this);
                if (!closed) onExit.run();
            }
        }, "blitzlink-" + label + "-out");
        out.setDaemon(true);
        out.start();
        Thread err = new Thread(() -> {
            try (BufferedReader r = new BufferedReader(new InputStreamReader(process.getErrorStream(), StandardCharsets.UTF_8))) {
                String line;
                while ((line = r.readLine()) != null) BlitzLink.LOGGER.info("[{}] {}", label, line);
            } catch (IOException ignored) {
            }
        }, "blitzlink-" + label + "-err");
        err.setDaemon(true);
        err.start();
    }

    /** Starts the sidecar with {@code mode} ("host"/"join") and its arguments. {@code --json} is always added. */
    public static TunnelProcess start(String mode, List<String> args, Path workDir, Consumer<JsonObject> onEvent, Runnable onExit) throws IOException {
        List<String> cmd = new ArrayList<>();
        cmd.add(NativeBundle.executable().toString());
        cmd.add(mode);
        cmd.addAll(args);
        cmd.add("--json");
        ProcessBuilder pb = new ProcessBuilder(cmd).directory(workDir.toFile());
        BlitzLink.LOGGER.debug("BlitzLink: starting {}", String.join(" ", cmd));
        return new TunnelProcess(pb.start(), "tunnel-" + mode, onEvent, onExit);
    }

    public void send(String type, JsonObject body) {
        if (closed) return;
        JsonObject o = body == null ? new JsonObject() : body.deepCopy();
        o.addProperty("t", type);
        byte[] line = (GSON.toJson(o) + "\n").getBytes(StandardCharsets.UTF_8);
        synchronized (stdin) {
            try {
                stdin.write(line);
                stdin.flush();
            } catch (IOException e) {
                BlitzLink.LOGGER.debug("[{}] stdin closed: {}", label, e.toString());
            }
        }
    }

    public boolean isAlive() {
        return !closed && process.isAlive();
    }

    @Override
    public void close() {
        if (closed) return;
        closed = true;
        LIVE.remove(this);
        send0quit();
        process.destroy();
        try {
            if (!process.waitFor(2, java.util.concurrent.TimeUnit.SECONDS)) process.destroyForcibly();
        } catch (InterruptedException e) {
            process.destroyForcibly();
            Thread.currentThread().interrupt();
        }
    }

    private void send0quit() {
        synchronized (stdin) {
            try {
                stdin.write("{\"t\":\"quit\"}\n".getBytes(StandardCharsets.UTF_8));
                stdin.flush();
                stdin.close();
            } catch (IOException ignored) {
            }
        }
    }

    public static String str(JsonObject o, String key, String fallback) {
        return o.has(key) && !o.get(key).isJsonNull() ? o.get(key).getAsString() : fallback;
    }

    public static int num(JsonObject o, String key, int fallback) {
        return o.has(key) && o.get(key).isJsonPrimitive() ? o.get(key).getAsInt() : fallback;
    }
}
