package controller

import (
	"net/http"

	"github.com/gin-gonic/gin"
	"github.com/labring/aiproxy/core/common"
	"github.com/labring/aiproxy/core/common/config"
	"github.com/labring/aiproxy/core/model"
	"github.com/labring/aiproxy/core/relay/adaptor"
	"github.com/labring/aiproxy/core/relay/meta"
	monitorplugin "github.com/labring/aiproxy/core/relay/plugin/monitor"
	"github.com/sirupsen/logrus"
)

// HandleResult contains all the information needed for consumption recording
type HandleResult struct {
	Error        adaptor.Error
	Usage        model.Usage
	UsageContext model.UsageContext
	UpstreamID   string
	AsyncUsage   bool
	BodyDetail   *BodyDetail
}

func ShouldSkipRequestBodyDetailForStatus(statusCode int) bool {
	if !monitorplugin.ChannelStatusHasPermission(statusCode) {
		return true
	}

	switch statusCode {
	case http.StatusMethodNotAllowed,
		http.StatusTooManyRequests:
		return true
	default:
		return false
	}
}

func logHandleError(
	log *logrus.Entry,
	respErr adaptor.Error,
	detail *BodyDetail,
	debugEnabled bool,
) {
	if detail == nil || !debugEnabled {
		log.Errorf("handle failed: %+v", respErr)
		return
	}

	switch {
	case detail.RequestBody != "" && detail.ResponseBody != "":
		log.Errorf(
			"handle failed: %+v\nrequest detail:\n%s\nresponse detail:\n%s",
			respErr,
			detail.RequestBody,
			detail.ResponseBody,
		)
	case detail.RequestBody != "":
		log.Errorf(
			"handle failed: %+v\nrequest detail:\n%s",
			respErr,
			detail.RequestBody,
		)
	case detail.ResponseBody != "":
		log.Errorf(
			"handle failed: %+v\nresponse detail:\n%s",
			respErr,
			detail.ResponseBody,
		)
	default:
		log.Errorf("handle failed: %+v", respErr)
	}
}

func Handle(
	adaptor adaptor.Adaptor,
	c *gin.Context,
	meta *meta.Meta,
	store adaptor.Store,
	opts ...BodyDetailOption,
) *HandleResult {
	log := common.GetLogger(c)

	result, detail, respErr := DoHelper(adaptor, c, meta, store, opts...)
	if respErr != nil {
		logHandleError(log, respErr, detail, config.DebugEnabled)

		return &HandleResult{
			Error:        respErr,
			Usage:        result.Usage,
			UsageContext: result.UsageContext,
			UpstreamID:   result.UpstreamID,
			AsyncUsage:   result.AsyncUsage,
			BodyDetail:   detail,
		}
	}

	return &HandleResult{
		Usage:        result.Usage,
		UsageContext: result.UsageContext,
		UpstreamID:   result.UpstreamID,
		AsyncUsage:   result.AsyncUsage,
		BodyDetail:   detail,
	}
}
