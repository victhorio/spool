FROM golang:1.27-bookworm AS build
WORKDIR /source
ADD https://github.com/tailscale/tailscale/archive/27047635f237d1f3e0b08c0f487674757f30d2f7.tar.gz /tmp/source.tar.gz
RUN tar -xzf /tmp/source.tar.gz --strip-components=1 && rm /tmp/source.tar.gz
COPY dev/reader-control/main.go ./cmd/spool-reader-test-control/main.go
RUN CGO_ENABLED=0 go build -o /control ./cmd/spool-reader-test-control

FROM scratch
COPY --from=build /control /control
ENTRYPOINT ["/control"]
