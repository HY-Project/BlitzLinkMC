package com.hyproj.blitzlink.bukkit;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.HexFormat;
import java.util.List;
import java.util.Locale;
import java.util.function.Consumer;
import java.util.logging.Logger;

import com.google.gson.Gson;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;

/**
 * The {@code blitzlink-tunnel host} sidecar for Bukkit — the same binary and JSON-line control as the mod
 * (common/.../tunnel/TunnelProcess.java, docs/PROTOCOL.md), unpacked from this jar's {@code natives/}.
 */
final class BukkitTunnel implements AutoCloseable {
    private static final Gson GSON = new Gson();

    private final Process process;
    private final OutputStream stdin;
    private volatile boolean closed;

    private BukkitTunnel(Process p, Logger log, Consumer<JsonObject> onEvent, Runnable onExit) {
        this.process = p;
        this.stdin = p.getOutputStream();
        Thread out = new Thread(() -> {
            try (BufferedReader r = new BufferedReader(new InputStreamReader(p.getInputStream(), StandardCharsets.UTF_8))) {
                String line;
                while ((line = r.readLine()) != null) {
                    if (line.isBlank()) continue;
                    try {
                        onEvent.accept(JsonParser.parseString(line).getAsJsonObject());
                    } catch (RuntimeException e) {
                        log.fine("[tunnel] " + line);
                    }
                }
            } catch (IOException ignored) {
            } finally {
                if (!closed) onExit.run();
            }
        }, "blitzlink-tunnel-out");
        out.setDaemon(true);
        out.start();
        Thread err = new Thread(() -> {
            try (BufferedReader r = new BufferedReader(new InputStreamReader(p.getErrorStream(), StandardCharsets.UTF_8))) {
                String line;
                while ((line = r.readLine()) != null) log.info("[tunnel] " + line);
            } catch (IOException ignored) {
            }
        }, "blitzlink-tunnel-err");
        err.setDaemon(true);
        err.start();
    }

    static BukkitTunnel start(Path dataDir, List<String> args, Logger log, Consumer<JsonObject> onEvent, Runnable onExit) throws IOException {
        List<String> cmd = new ArrayList<>();
        cmd.add(executable(dataDir).toString());
        cmd.add("host");
        cmd.addAll(args);
        cmd.add("--json");
        return new BukkitTunnel(new ProcessBuilder(cmd).directory(dataDir.toFile()).start(), log, onEvent, onExit);
    }

    void send(String type, JsonObject body) {
        if (closed) return;
        JsonObject o = body == null ? new JsonObject() : body.deepCopy();
        o.addProperty("t", type);
        byte[] line = (GSON.toJson(o) + "\n").getBytes(StandardCharsets.UTF_8);
        synchronized (stdin) {
            try {
                stdin.write(line);
                stdin.flush();
            } catch (IOException ignored) {
            }
        }
    }

    boolean isAlive() {
        return !closed && process.isAlive();
    }

    @Override
    public void close() {
        if (closed) return;
        send("quit", null);
        closed = true;
        process.destroy();
        try {
            if (!process.waitFor(2, java.util.concurrent.TimeUnit.SECONDS)) process.destroyForcibly();
        } catch (InterruptedException e) {
            process.destroyForcibly();
            Thread.currentThread().interrupt();
        }
    }

    // ---- sidecar binary (natives/index.txt: `<platform> <file> <sha256>`) ----

    private static Path executable(Path dataDir) throws IOException {
        String os = System.getProperty("os.name", "").toLowerCase(Locale.ROOT);
        String arch = System.getProperty("os.arch", "").toLowerCase(Locale.ROOT);
        boolean x64 = arch.equals("amd64") || arch.equals("x86_64");
        boolean arm64 = arch.equals("aarch64") || arch.equals("arm64");
        String platform = os.contains("win") && x64 ? "windows-x64" : os.contains("win") && arm64 ? "windows-arm64" : os.contains("linux") && x64 ? "linux-x64" : os.contains("linux") && arm64 ? "linux-arm64" : null;
        if (platform == null) throw new IOException("unsupported platform " + os + "/" + arch);
        String line;
        try (InputStream in = BukkitTunnel.class.getResourceAsStream("/natives/index.txt")) {
            if (in == null) throw new IOException("this BlitzLink build has no sidecar binaries");
            line = new String(in.readAllBytes(), StandardCharsets.UTF_8).lines().filter(l -> l.startsWith(platform + " ")).findFirst()
                    .orElseThrow(() -> new IOException("no sidecar for " + platform));
        }
        String[] parts = line.split(" ");
        Path dir = dataDir.resolve("bin").resolve(parts[2].substring(0, 16));
        Path exe = dir.resolve(parts[1]);
        if (!Files.exists(exe) || !parts[2].equals(sha256(exe))) {
            Files.createDirectories(dir);
            try (InputStream in = BukkitTunnel.class.getResourceAsStream("/natives/" + platform + "/" + parts[1])) {
                if (in == null) throw new IOException("no sidecar for " + platform);
                Files.copy(in, exe, StandardCopyOption.REPLACE_EXISTING);
            }
            if (!parts[2].equals(sha256(exe))) {
                Files.deleteIfExists(exe);
                throw new IOException("sidecar checksum mismatch");
            }
        }
        exe.toFile().setExecutable(true);
        return exe;
    }

    private static String sha256(Path p) throws IOException {
        try {
            return HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(Files.readAllBytes(p)));
        } catch (java.security.NoSuchAlgorithmException e) {
            throw new IOException(e);
        }
    }
}
