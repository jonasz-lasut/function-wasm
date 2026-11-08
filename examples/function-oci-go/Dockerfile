# The hello function as an image function-oci boots: a static Go binary,
# cross-compiled on the build platform for the target, in an empty image.
# The process contract needs the image's command and nothing else; the
# runtime mounts a fresh /tmp itself. The Go version is go.mod's.
FROM --platform=${BUILDPLATFORM} golang:1.27.3 AS build
ARG TARGETOS
ARG TARGETARCH
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY . .
RUN CGO_ENABLED=0 GOOS=${TARGETOS} GOARCH=${TARGETARCH} go build -trimpath -ldflags='-s -w' -o /function .

FROM scratch
COPY --from=build /function /function
# A numeric user needs no /etc/passwd in the image; the runtime resolves it
# as a container runtime does.
USER 65532:65532
ENTRYPOINT ["/function"]
