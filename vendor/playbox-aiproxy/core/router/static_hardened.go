//go:build playbox_hardened

package router

import "github.com/gin-gonic/gin"

// SetStaticFileRouter is compiled out in playbox_hardened builds; Playbox Hub PWA is the sole UI.
func SetStaticFileRouter(_ *gin.Engine) {}
