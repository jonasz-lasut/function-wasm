package main

import "strconv"

// logSink delivers one wasmfn.log record; the wasip1 build points it at the
// host import, other builds print to stderr. The payload is built by hand
// rather than with encoding/json to keep reflection out of the module.
var logSink = stderrSink

func logInfo(msg string, keysAndValues ...string) {
	payload := `{"msg":` + strconv.Quote(msg) + `,"kv":[`
	for i, kv := range keysAndValues {
		if i > 0 {
			payload += ","
		}
		payload += strconv.Quote(kv)
	}
	payload += "]}"
	logSink(0, []byte(payload))
}
