module github.com/jonasz-lasut/function-wasm/examples/hello-tinygo

go 1.26.6

require (
	github.com/google/go-cmp v0.7.0
	github.com/planetscale/vtprotobuf v0.6.0
	google.golang.org/protobuf v1.36.12
)

tool (
	github.com/planetscale/vtprotobuf/cmd/protoc-gen-go-vtproto
	google.golang.org/protobuf/cmd/protoc-gen-go
)
