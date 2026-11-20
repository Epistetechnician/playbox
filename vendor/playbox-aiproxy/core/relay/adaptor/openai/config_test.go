//nolint:testpackage
package openai

import (
	"testing"
	"time"

	"github.com/labring/aiproxy/core/model"
	"github.com/labring/aiproxy/core/relay/meta"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestResponsesFirstEventTimeoutConfig(t *testing.T) {
	t.Parallel()

	a := &Adaptor{}

	defaultCfg, err := a.loadConfig(&meta.Meta{})
	require.NoError(t, err)
	assert.Equal(t, 2*time.Second, defaultCfg.responsesFirstEventTimeout())

	configured, err := a.loadConfig(&meta.Meta{
		ChannelConfigs: model.ChannelConfigs{
			"responses_first_event_timeout": 30,
		},
	})
	require.NoError(t, err)
	assert.Equal(t, 30*time.Second, configured.responsesFirstEventTimeout())
}

func TestConfigSchemaIncludesResponsesFirstEventTimeout(t *testing.T) {
	t.Parallel()

	properties, ok := configSchema()["properties"].(map[string]any)
	require.True(t, ok)

	field, ok := properties["responses_first_event_timeout"].(map[string]any)
	require.True(t, ok)
	assert.Equal(t, "integer", field["type"])
	assert.Equal(t, defaultResponsesFirstEventTimeoutSeconds, field["default"])
	assert.Equal(t, 0, field["minimum"])
}
