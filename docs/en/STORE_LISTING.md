# Store listing sheet: Modrinth and CurseForge

The fields to fill in when publishing. The project description body is [STORE_DESCRIPTION.md](STORE_DESCRIPTION.md); paste it as-is.
It uses plain Markdown only (headings, lists, bold, links; no HTML and no tables), so it renders the same on both sites.
한국어: [../ko/STORE_LISTING.md](../ko/STORE_LISTING.md)

## Common

| Field | Value |
|---|---|
| Name | BlitzLink |
| Slug | `blitzlink` |
| Summary (short, ≤ 100 chars) | Join any Minecraft server with one code. Peer-to-peer, usually no port forwarding. |
| Summary (long, ≤ 256 chars) | Share a join code like MCJE-7K3Q and friends join from the multiplayer screen. The relay only introduces you; game traffic flows directly over WebRTC. Mod list check before joining, real player IPs, NeoForge + Fabric in one jar. |
| Game version | 26.3 |
| License | Apache-2.0 |
| Source | https://github.com/HY-Project/BlitzLinkMC |
| Issues | https://github.com/HY-Project/BlitzLinkMC/issues |
| Website | https://hyproj.com |
| Icon | `common/src/main/resources/assets/blitzlink/icon.png` |

## Modrinth

| Field | Value |
|---|---|
| Project type | Mod (the Bukkit plugin can be another file in the same project, loaders `bukkit` · `spigot` · `paper` · `purpur`) |
| Categories | Social, Utility, Management |
| Environment | Client: **required** (to join by code) · Server: **optional** (only needed to host on a dedicated server) |
| Loaders (mod jar) | NeoForge, Fabric |
| Dependencies | Fabric API: required (Fabric only) |

## CurseForge

| Field | Value |
|---|---|
| Project (mod) | Minecraft → Mods. Categories: **Server Utility**, **Utility & QoL** |
| Project (plugin) | Minecraft → **Bukkit Plugins**, a separate project for the plugin jar. Category: Admin Tools / General |
| Mod loaders (mod jar) | NeoForge, Fabric |
| Relations | Fabric API: Required Dependency |

## Files to upload

| File | Where | Loaders |
|---|---|---|
| `blitzlink-<version>+mc26.3.jar` | Modrinth mod project, CurseForge mod project | NeoForge, Fabric (one file for both) |
| `blitzlink-bukkit-26.3-<version>.jar` | Modrinth (same project) or CurseForge Bukkit Plugins project | Bukkit, Spigot, Paper, Purpur |

Both come from the GitHub Actions `build` workflow. Only upload builds made with the `BLITZLINK_NET_JSON` repository variable set: a build
from the placeholder `data/net.json` has no relay and shows "No relay server is configured".

## Review notes (bundled executable)

Both sites review projects that contain native executables. Put this in the review notes (Modrinth) or the file comment (CurseForge):

> BlitzLink bundles a native helper (`blitzlink-tunnel`, Windows x64/ARM64 and Linux x64/ARM64) inside the jar. It is not downloaded at
> runtime. It is extracted to `.blitzlink/bin/<hash>/` and its SHA-256 is checked against the jar's list before every run. It is built from the open-source code in
> `native/` of https://github.com/HY-Project/BlitzLinkMC by the public GitHub Actions workflow, and `natives/SHA256SUMS` in each build
> lists the hashes. It handles the WebRTC peer-to-peer connection and contacts only the configured relay, STUN servers and the peer
> the player chose to join.

## Screenshots to prepare

1. The BlitzLink multiplayer screen with the public list
2. The server card with the mod check (red ✕ rows and green ✓ rows)
3. Open Server with a join code shown
4. A dedicated server console showing `BlitzLink: join code MCJE-…`
5. Advanced Settings (custom relay)
