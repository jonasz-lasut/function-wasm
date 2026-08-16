//go:build generate

// Regenerates internal/fnv1 from proto/run_function.proto: the protobuf-go
// message types plus vtprotobuf's reflection-free MarshalVT/UnmarshalVT, which
// is what makes the messages usable under TinyGo (protobuf-go's own codec
// needs reflect.New, which TinyGo does not implement).
//
// `go generate ./...` needs only protoc: the plugins are built from this
// module's go.mod tool directives into .bin/, and protoc's version stamp —
// which differs by environment and carries no information — is stripped.
//
//go:generate go build -o .bin/protoc-gen-go google.golang.org/protobuf/cmd/protoc-gen-go
//go:generate go build -o .bin/protoc-gen-go-vtproto github.com/planetscale/vtprotobuf/cmd/protoc-gen-go-vtproto
//go:generate protoc --plugin=protoc-gen-go=.bin/protoc-gen-go --plugin=protoc-gen-go-vtproto=.bin/protoc-gen-go-vtproto -I proto --go_out=. --go_opt=module=github.com/jonasz-lasut/function-wasm/examples/hello-tinygo --go_opt=Mrun_function.proto=github.com/jonasz-lasut/function-wasm/examples/hello-tinygo/internal/fnv1;fnv1 --go-vtproto_out=. --go-vtproto_opt=module=github.com/jonasz-lasut/function-wasm/examples/hello-tinygo --go-vtproto_opt=Mrun_function.proto=github.com/jonasz-lasut/function-wasm/examples/hello-tinygo/internal/fnv1;fnv1 --go-vtproto_opt=features=marshal+unmarshal+size run_function.proto
//go:generate sh -c "sed -i.bak '/^\\/\\/ \tprotoc  /d' internal/fnv1/run_function.pb.go && rm -f internal/fnv1/*.bak"

package main
