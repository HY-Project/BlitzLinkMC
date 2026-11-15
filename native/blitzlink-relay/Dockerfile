# blitzlink-relay — BlitzLink 공식 중계 이미지(hyproj.com · linux/arm64). 배포 안내는 docs/en/RELAY.md(사용자 승인 뒤에만).
# 빌드 맥락은 **BlitzLink_26.3 뿌리**다(blitzlink-core가 data/net.json을 품는다):
#   docker buildx build --platform linux/arm64 -f native/blitzlink-relay/Dockerfile -t blitzlink-relay:0.1.0 .
FROM rust:1.99.0-slim-trixie AS build
WORKDIR /src
COPY native native
COPY data data
RUN cargo build --release --locked --manifest-path native/Cargo.toml -p blitzlink-relay

FROM debian:trixie-slim
RUN useradd --system --uid 10001 --no-create-home relay
COPY --from=build /src/native/target/release/blitzlink-relay /usr/local/bin/blitzlink-relay
USER relay
# 1947/tcp — HTTP·WebSocket(nginx 뒤, 밖에 열지 않는다). STUN은 hyproj.com에서 이미 도는 3478을 같이 쓰므로 끈다.
EXPOSE 1947/tcp
ENTRYPOINT ["blitzlink-relay"]
CMD ["--listen", "0.0.0.0:1947", "--no-stun", "--trust-proxy"]
