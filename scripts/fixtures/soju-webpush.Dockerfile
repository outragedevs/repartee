FROM golang:1.27.1
RUN apt-get update && apt-get install -y --no-install-recommends python3 python3-cryptography openssl && rm -rf /var/lib/apt/lists/*
COPY source /soju
WORKDIR /soju
RUN make soju
