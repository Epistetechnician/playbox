//go:build playbox_hardened

package controller

import (
	"context"

	"github.com/labring/aiproxy/core/model"
	"github.com/labring/aiproxy/core/relay/plugin"
	"github.com/labring/aiproxy/core/relay/plugin/noop"
)

// newWebSearchPlugin compiles out the upstream web-search plugin under playbox_hardened
// so external web fetches cannot bypass Playbox's witness taint boundary.
func newWebSearchPlugin(_ context.Context, _ *model.ModelCaches) plugin.Plugin {
	return &noop.Noop{}
}
