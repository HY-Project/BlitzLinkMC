# Store listing sheet: Modrinth and CurseForge

The fields to fill in when publishing. There are two editions of the listing:

- **CurseForge and GitHub** (the default): description [STORE_DESCRIPTION.md](STORE_DESCRIPTION.md), the generated pixel-art icon,
  banner and loader badges, and the default jar.
- **Modrinth** (**BlitzLink-HAE**, Human Art Edition): Modrinth does not allow AI-generated project art, so it gets
  [STORE_DESCRIPTION_MODRINTH.md](STORE_DESCRIPTION_MODRINTH.md), the hand-drawn icon from `branding/modrinth/`, in-game
  screenshots instead of the banner and badges, and the `BlitzLink-HAE-<version>+mc26.3.jar` jar (same code, hand-drawn mod icon).

Paste the description as-is.
It uses plain Markdown only (headings, lists, bold, links, images; no HTML and no tables), so it renders the same on both sites.
The banner, loader badges and screenshots are linked from `branding/` and `docs/screenshots/` on the `mc-26.3` branch through
raw.githubusercontent.com, so push both before pasting. If a site will not show an external image, upload the PNG there and swap the URL.
한국어: [../ko/STORE_LISTING.md](../ko/STORE_LISTING.md)

## Common

| Field | Value |
|---|---|
| Name | BlitzLink |
| Slug | `blitzlink` |
| Summary (short, ≤ 100 chars) | Join any Minecraft server with one code. Peer-to-peer, usually no port forwarding. (CurseForge; Modrinth has its own below) |
| Summary (long, ≤ 256 chars) | Share a join code like MCJE-7K3Q and friends join from the multiplayer screen. The relay only introduces you; game traffic flows directly over WebRTC. Mod list check before joining, real player IPs, NeoForge + Fabric in one jar. |
| Game version | 26.3 |
| License | Apache-2.0 |
| Source | https://github.com/HY-Project/BlitzLinkMC |
| Issues | https://github.com/HY-Project/BlitzLinkMC/issues |
| Website | https://hyproj.com |

## Modrinth

| Field | Value |
|---|---|
| Project type | Mod (the Bukkit plugin can be another file in the same project, loaders `bukkit` · `spigot` · `paper` · `purpur`) |
| Categories | Social, Utility, Management |
| Environment | Client: **required** (to join by code) · Server: **optional** (only needed to host on a dedicated server) |
| Loaders (mod jar) | NeoForge, Fabric |
| Dependencies | Fabric API: required (Fabric only) |
| Summary | Join any Minecraft server with one code. Peer-to-peer, usually no port forwarding. (Modrinth Exclusive Human Art Edition) (121 chars; Modrinth allows 256) |
| Description | [STORE_DESCRIPTION_MODRINTH.md](STORE_DESCRIPTION_MODRINTH.md). Its first line marks the edition: 🎨 **Modrinth Exclusive: Human Art Edition** |
| Icon | `branding/modrinth/blitzlink-icon-hand-drawn-512.png` (drawn by hand in Piskel; the same picture is the mod icon inside the BlitzLink-HAE jar) |
| Gallery | The five screenshots in `docs/screenshots/`, in the order below; **01 is the featured image**. No banner or badges here (they are generated art) |
| Mod file | `BlitzLink-HAE-<version>+mc26.3.jar` |

## CurseForge

| Field | Value |
|---|---|
| Project (mod) | Minecraft → Mods. Categories: **Server Utility**, **Utility & QoL** |
| Project (plugin) | Minecraft → **Bukkit Plugins**, a separate project for the plugin jar. Category: Admin Tools / General |
| Mod loaders (mod jar) | NeoForge, Fabric |
| Relations | Fabric API: Required Dependency |
| Description | [STORE_DESCRIPTION.md](STORE_DESCRIPTION.md) (banner, badges and the screenshots inline) |
| Logo | `branding/blitzlink-icon-512.png` (same artwork as the in-game `assets/blitzlink/icon.png` of the default jar) |
| Banner | `branding/blitzlink-banner-4x.png` |
| Images | The five screenshots in `docs/screenshots/`. If the uploader refuses `.webp`, convert 04 and 05 to PNG first |
| Mod file | `blitzlink-<version>+mc26.3.jar` |

## Files to upload

| File | Where | Loaders |
|---|---|---|
| `blitzlink-<version>+mc26.3.jar` | CurseForge mod project, GitHub release | NeoForge, Fabric (one file for both) |
| `BlitzLink-HAE-<version>+mc26.3.jar` | Modrinth mod project | NeoForge, Fabric (one file for both) |
| `blitzlink-bukkit-26.3-<version>.jar` | Modrinth (same project) or CurseForge Bukkit Plugins project | Bukkit, Spigot, Paper, Purpur |

All of them come from the GitHub Actions `build` workflow. Only upload builds made with the `BLITZLINK_NET_JSON` repository variable set: a build
from the placeholder `data/net.json` has no relay and shows "No relay server is configured".

## Review notes (bundled executable)

Both sites review projects that contain native executables. Put this in the review notes (Modrinth) or the file comment (CurseForge):

> BlitzLink bundles a native helper (`blitzlink-tunnel`, Windows x64/ARM64 and Linux x64/ARM64) inside the jar. It is not downloaded at
> runtime. It is extracted to `.blitzlink/bin/<hash>/` and its SHA-256 is checked against the jar's list before every run. It is built from the open-source code in
> `native/` of https://github.com/HY-Project/BlitzLinkMC by the public GitHub Actions workflow, and `natives/SHA256SUMS` in each build
> lists the hashes. It handles the WebRTC peer-to-peer connection and contacts only the configured relay, STUN servers and the peer
> the player chose to join.

## Screenshots

In `docs/screenshots/`, shown in this order on Modrinth (gallery + description), CurseForge and the README:

| File | Title | Caption |
|---|---|---|
| `01-relay-list.png` | Join with a code | The BlitzLink multiplayer screen: join code box and the public server list. A server for another loader is marked red with ✕. |
| `02-server-card.png` | Check before you join | The server card: version, loader, identity and the server's mod list, fetched straight from the server. |
| `03-loader-mismatch.png` | Wrong loader? You'll know | The same Fabric server seen from a NeoForge client: the card turns red and says why. |
| `04-pause-menu.webp` | Open your world | The BlitzLink button sits in the pause menu where "Open to LAN" used to be. |
| `05-playing-together.webp` | Then just play | Two players in the same world, connected peer-to-peer. |

Still worth taking later: Open Server with a join code shown, a dedicated server console with `BlitzLink: join code MCJE-…`, and
Advanced Settings (custom relay).
