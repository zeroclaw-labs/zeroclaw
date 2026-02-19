FROM golang:1.22-alpine AS build
WORKDIR /app
COPY . .
RUN go mod tidy
RUN go build -o zc-bridge

FROM alpine:3.19
WORKDIR /app
COPY --from=build /app/zc-bridge .
EXPOSE 18789
CMD ["./zc-bridge"]