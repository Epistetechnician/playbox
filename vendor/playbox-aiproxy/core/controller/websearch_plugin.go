//go:build !playbox_hardened

package controller

import (
	"context"

	"github.com/labring/aiproxy/core/model"
	"github.com/labring/aiproxy/core/relay/plugin"
	websearch "github.com/labring/aiproxy/core/relay/plugin/web-search"
)

func newWebSearchPlugin(ctx context.Context, mc *model.ModelCaches) plugin.Plugin {
	return websearch.NewWebSearchPlugin(func(modelName string) (*model.Channel, error) {
		return getWebSearchChannel(ctx, mc, modelName)
	})
}
