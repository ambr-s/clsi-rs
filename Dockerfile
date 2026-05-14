# syntax=docker/dockerfile:1.7
# ---- build stage ----
FROM rust:1.91-slim-bookworm AS build
WORKDIR /src

# Prime the cargo registry/index in its own layer so source edits don't
# re-fetch dependencies.
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs \
 && cargo build --release \
 && rm -rf src target/release/deps/clsi_rs* target/release/clsi-rs* 2>/dev/null || true

COPY src ./src
RUN cargo build --release

# ---- runtime stage ----
# Debian + texlive packages instead of the full ~5GB texlive/texlive image.
# Trades coverage of obscure packages for a smaller image (= faster cold pulls).
FROM debian:bookworm-slim
ARG UID=10001

RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        ca-certificates \
        latexmk \
        texlive-latex-base \
        texlive-latex-recommended \
        texlive-latex-extra \
        texlive-fonts-recommended \
        texlive-science \
        texlive-bibtex-extra \
        texlive-xetex \
        texlive-luatex \
        biber \
 && rm -rf /var/lib/apt/lists/* /var/cache/apt/archives/* \
 # Rebuild the TeX filename database so kpsewhich can find files in packages
 # installed after texlive-base (e.g. scrartcl.cls from koma-script in
 # texlive-latex-recommended). Without this, pdflatex reports "file not found"
 # for things that are physically present on disk.
 && mktexlsr

# Non-root user — CF Containers run unprivileged; fly/Modal happy with it too.
RUN useradd -u "$UID" -m -s /usr/sbin/nologin clsi \
 && mkdir -p /work \
 && chown clsi:clsi /work

COPY --from=build /src/target/release/clsi-rs /usr/local/bin/clsi-rs
USER clsi
WORKDIR /work
EXPOSE 3013
ENV CLSI_BIND=0.0.0.0:3013 \
    CLSI_WORK_DIR=/work
ENTRYPOINT ["/usr/local/bin/clsi-rs"]
