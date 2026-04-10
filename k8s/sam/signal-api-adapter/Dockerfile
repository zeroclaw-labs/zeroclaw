FROM golang:1.22.12-alpine3.21 AS build
WORKDIR /src
COPY go.mod ./
COPY go.sum ./
COPY main.go ./
RUN CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -trimpath -ldflags='-s -w' -o /out/signal-api-adapter .

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /out/signal-api-adapter /signal-api-adapter
EXPOSE 8686
ENV LISTEN_ADDR=:8686
ENTRYPOINT ["/signal-api-adapter"]
