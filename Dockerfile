FROM krinkin/rv64-toolchain:latest

RUN apt-get update && apt-get install -y curl pkg-config libssl-dev && \
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && \
    apt-get clean && rm -rf /var/lib/apt/lists/*
ENV PATH="/root/.cargo/bin:$PATH"

COPY . /app

WORKDIR /app/risc-v-sim
RUN stat Cargo.toml
RUN cargo build --release
RUN cp target/release/risc-v-sim /usr/local/bin/
RUN risc-v-sim --help

WORKDIR /app
RUN cargo build --release

ENV SIMULATOR_BINARY="/usr/local/bin/risc-v-sim"
ENV AS_BINARY="riscv64-linux-gnu-as"
ENV LD_BINARY="riscv64-linux-gnu-ld"
ENV CODESIZE_MAX="2048"
ENV TICKS_MAX="128"
ENV MONGODB_URI="mongodb://localhost:27017"
ENV MONGODB_DB="riscv_sim"
ENV SUBMISSIONS_FOLDER="submission"

ENV GITHUB_CLIENT_ID=""
ENV GITHUB_CLIENT_SECRET=""

ENV JWT_SECRET=""

RUN mkdir -p /app/submission

EXPOSE 3000

ENTRYPOINT ["./target/release/risc-v-sim-web"]
