//go:build playbox_hardened

package router

import "github.com/gin-gonic/gin"

// SetSwaggerRouter is compiled out in playbox_hardened builds.
func SetSwaggerRouter(_ *gin.Engine) {}
