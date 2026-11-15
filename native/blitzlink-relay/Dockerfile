# blitzlink-relay — BlitzLink 공식 중계 이미지(hyproj.com · linux/arm64). 배포 안내는 docs/en/RELAY.md(사용자 승인 뒤에만).
# 빌드 맥락은 **BlitzLink_26.3 뿌리**다(blitzlink-core가 data/net.json을 품는다):
#   docker buildx build --platform linux/arm64 -f native/blitzlink-relay/Dockerfile -t blitzlink-relay:0.1.0 .
FROM rust:1.99.0-slim-trixie AS build
# 워크스페이스 release 프로필(fat LTO·codegen-units 1)은 사이드카용이다. OCI 무료 ARM에서 그대로 지으면 한 시간을
# 넘긴다(Blitzkrieg 중계가 같은 일로 배포 SSH 40분 제한에 걸렸다). 중계는 SDP만 건네므로 이 이미지에서만 낮춘다.
ENV CARGO_PROFILE_RELEASE_LTO=false \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
    CARGO_PROFILE_RELEASE_OPT_LEVEL=2
WORKDIR /src
COPY native native
COPY data data
# registry·target을 캐시 마운트에 남겨 REF를 올릴 때 바뀐 크레이트만 다시 짓는다(target은 레이어에 없으므로 바이너리를 꺼내 둔다).
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,id=blitzlink-relay-target,target=/src/native/target \
    cargo build --release --locked --manifest-path native/Cargo.toml -p blitzlink-relay \
    && cp native/target/release/blitzlink-relay /blitzlink-relay

FROM debian:trixie-slim
RUN useradd --system --uid 10001 --no-create-home relay
COPY --from=build /blitzlink-relay /usr/local/bin/blitzlink-relay
USER relay
# 1947/tcp — HTTP·WebSocket(nginx 뒤, 밖에 열지 않는다). STUN은 hyproj.com에서 이미 도는 3478을 같이 쓰므로 끈다.
EXPOSE 1947/tcp
ENTRYPOINT ["blitzlink-relay"]
CMD ["--listen", "0.0.0.0:1947", "--no-stun", "--trust-proxy"]
