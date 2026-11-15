# 스토어 등록 시트 — Modrinth · CurseForge

올릴 때 채울 칸. 프로젝트 본문은 [STORE_DESCRIPTION.md](STORE_DESCRIPTION.md)를 그대로 붙여 넣는다. 제목·목록·굵게·링크·이미지만 쓴
평범한 Markdown이라(HTML·표 없음) 두 사이트에서 똑같이 보인다. 두 사이트 모두 영어 본문이 기본이니 공식 본문은
[../en/STORE_DESCRIPTION.md](../en/STORE_DESCRIPTION.md), 한국어판은 한국 커뮤니티·추가 설명용이다.
맨 위 배너와 로더 배지는 `mc-26.3` 브랜치의 `branding/`을 raw.githubusercontent.com 주소로 불러오므로, 붙여 넣기 전에 `branding/`을
푸시해 둔다. 사이트가 외부 이미지를 막으면 그 사이트에 PNG를 직접 올리고 주소만 바꾼다.
English: [../en/STORE_LISTING.md](../en/STORE_LISTING.md)

## 공통

| 칸 | 값 |
|---|---|
| 이름 | BlitzLink |
| 슬러그 | `blitzlink` |
| 요약(짧게, 100자 이하) | Join any Minecraft server with one code. Peer-to-peer, usually no port forwarding. |
| 요약(길게, 256자 이하) | Share a join code like MCJE-7K3Q and friends join from the multiplayer screen. The relay only introduces you; game traffic flows directly over WebRTC. Mod list check before joining, real player IPs, NeoForge + Fabric in one jar. |
| 한국어 요약(참고) | 코드 하나로 마인크래프트 서버에 들어간다. P2P 직접 연결, 대개 포트포워딩 없이. |
| 게임 버전 | 26.3 |
| 라이선스 | Apache-2.0 |
| 소스 | https://github.com/HY-Project/BlitzLinkMC |
| 이슈 | https://github.com/HY-Project/BlitzLinkMC/issues |
| 웹사이트 | https://hyproj.com |
| 아이콘 | `branding/blitzlink-icon-512.png`(게임 안 `assets/blitzlink/icon.png`와 같은 그림) |
| 배너 | `branding/blitzlink-banner-4x.png`(Modrinth는 갤러리에도 올려 대표 이미지로 지정) |

## Modrinth

| 칸 | 값 |
|---|---|
| 프로젝트 종류 | Mod(Bukkit 플러그인은 같은 프로젝트에 파일을 더해 로더 `bukkit`·`spigot`·`paper`·`purpur`로 올릴 수 있다) |
| 카테고리 | Social, Utility, Management |
| 환경 | Client: **required**(코드로 참가) · Server: **optional**(전용 서버에서 열 때만) |
| 로더(모드 jar) | NeoForge, Fabric |
| 의존성 | Fabric API — required(Fabric만) |

## CurseForge

| 칸 | 값 |
|---|---|
| 프로젝트(모드) | Minecraft → Mods. 카테고리 **Server Utility**, **Utility & QoL** |
| 프로젝트(플러그인) | Minecraft → **Bukkit Plugins**에 플러그인 jar용 프로젝트를 따로. 카테고리 Admin Tools / General |
| 모드 로더(모드 jar) | NeoForge, Fabric |
| 관계 | Fabric API — Required Dependency |

## 올릴 파일

| 파일 | 어디에 | 로더 |
|---|---|---|
| `blitzlink-<version>+mc26.3.jar` | Modrinth 모드 프로젝트, CurseForge 모드 프로젝트 | NeoForge, Fabric(한 파일로 둘 다) |
| `blitzlink-bukkit-26.3-<version>.jar` | Modrinth(같은 프로젝트) 또는 CurseForge Bukkit Plugins 프로젝트 | Bukkit, Spigot, Paper, Purpur |

둘 다 GitHub Actions `build` 워크플로가 만든다. **저장소 변수 `BLITZLINK_NET_JSON`을 넣고 만든 빌드만** 올린다 — 자리표시 `data/net.json`으로
만든 빌드는 중계가 없어 「중계 서버가 설정되지 않았습니다」가 뜬다.

## 심사 메모(동봉 실행 파일)

두 사이트 모두 네이티브 실행 파일이 든 프로젝트를 따로 본다. Modrinth 심사 메모나 CurseForge 파일 설명에 아래를 적는다(영어 그대로).

> BlitzLink bundles a native helper (`blitzlink-tunnel`, Windows x64/ARM64 and Linux x64/ARM64) inside the jar. It is not downloaded at
> runtime. It is extracted to `.blitzlink/bin/<hash>/` and its SHA-256 is checked against the jar's list before every run. It is built from
> the open-source code in `native/` of https://github.com/HY-Project/BlitzLinkMC by the public GitHub Actions workflow, and
> `natives/SHA256SUMS` in each build lists the hashes. It handles the WebRTC peer-to-peer connection and contacts only the configured relay,
> STUN servers and the peer the player chose to join.

## 준비할 스크린샷

1. 공개 목록이 보이는 BlitzLink 멀티플레이 화면
2. 모드 확인이 보이는 서버 카드(빨간 ✕ 줄과 초록 ✓ 줄)
3. 참가 코드가 뜬 「서버 열기」
4. `BlitzLink: join code MCJE-…`가 찍힌 전용 서버 콘솔
5. 고급 설정(커스텀 중계)
