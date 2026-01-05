FROM ubuntu:22.04

RUN apt update && apt install -y \
    ca-certificates \
    curl \
    bash \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY config.json .
COPY ws-tcp-proxy .
COPY xmrig-proxy .
COPY entrypoint.sh .

RUN chmod +x ./ws-tcp-proxy ./xmrig-proxy ./entrypoint.sh

EXPOSE 8080 3333

ENTRYPOINT ["./entrypoint.sh"]
