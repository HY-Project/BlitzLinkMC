package com.hyproj.blitzlink.tunnel;

import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.HexFormat;
import java.util.Locale;
import java.util.Optional;

import com.hyproj.blitzlink.BlitzLink;
import com.hyproj.blitzlink.platform.Platform;

/**
 * Finds the {@code blitzlink-tunnel} sidecar for this OS/arch inside the mod jar ({@code natives/index.txt}, written by the
 * root {@code bundleNatives} task), unpacks it to {@code <gameDir>/.blitzlink/bin/<sha>/} and checks its SHA-256 before use.
 */
public final class NativeBundle {
    private static Path cached;

    private NativeBundle() {}

    /** windows-x64, windows-arm64, linux-x64, linux-arm64 — or empty on platforms without a bundled binary. */
    public static Optional<String> platform() {
        String os = System.getProperty("os.name", "").toLowerCase(Locale.ROOT);
        String arch = System.getProperty("os.arch", "").toLowerCase(Locale.ROOT);
        boolean x64 = arch.equals("amd64") || arch.equals("x86_64");
        boolean arm64 = arch.equals("aarch64") || arch.equals("arm64");
        if (os.contains("win") && x64) return Optional.of("windows-x64");
        if (os.contains("win") && arm64) return Optional.of("windows-arm64");
        if (os.contains("linux") && x64) return Optional.of("linux-x64");
        if (os.contains("linux") && arm64) return Optional.of("linux-arm64");
        return Optional.empty();
    }

    /** Dev override: {@code -Dblitzlink.tunnel=<path>} runs a locally built sidecar (dev runs only). */
    public static synchronized Path executable() throws IOException {
        if (cached != null && Files.isExecutable(cached)) return cached;
        if (Boolean.getBoolean("blitzlink.dev")) {
            String dev = System.getProperty("blitzlink.tunnel");
            if (dev != null && !dev.isBlank()) return cached = Path.of(dev);
        }
        String platform = platform().orElseThrow(() -> new IOException("unsupportedPlatform"));
        String line = indexLine(platform).orElseThrow(() -> new IOException("sidecarMissing"));
        String[] parts = line.split(" ");
        String name = parts[1];
        String sha = parts[2];
        Path dir = Platform.get().gameDir().resolve(".blitzlink").resolve("bin").resolve(sha.substring(0, 16));
        Path exe = dir.resolve(name);
        if (!Files.exists(exe) || !sha.equals(sha256(exe))) {
            Files.createDirectories(dir);
            try (InputStream in = NativeBundle.class.getResourceAsStream("/natives/" + platform + "/" + name)) {
                if (in == null) throw new IOException("sidecarMissing");
                Path tmp = Files.createTempFile(dir, name, ".part");
                Files.copy(in, tmp, StandardCopyOption.REPLACE_EXISTING);
                Files.move(tmp, exe, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
            }
            if (!sha.equals(sha256(exe))) {
                Files.deleteIfExists(exe);
                throw new IOException("sidecar checksum mismatch");
            }
        }
        if (!exe.toFile().canExecute() && !exe.toFile().setExecutable(true)) {
            BlitzLink.LOGGER.warn("BlitzLink: could not mark {} executable", exe);
        }
        return cached = exe;
    }

    private static Optional<String> indexLine(String platform) throws IOException {
        try (InputStream in = NativeBundle.class.getResourceAsStream("/natives/index.txt")) {
            if (in == null) return Optional.empty();
            return new String(in.readAllBytes(), StandardCharsets.UTF_8).lines()
                    .filter(l -> l.startsWith(platform + " ") && l.split(" ").length == 3)
                    .findFirst();
        }
    }

    private static String sha256(Path p) throws IOException {
        try {
            return HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(Files.readAllBytes(p)));
        } catch (NoSuchAlgorithmException e) {
            throw new IOException(e);
        }
    }
}
