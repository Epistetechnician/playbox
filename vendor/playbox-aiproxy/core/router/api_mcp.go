//go:build !playbox_hardened

package router

import (
	"github.com/gin-gonic/gin"
	mcp "github.com/labring/aiproxy/core/controller/mcp"
)

func setGroupMCPAPIRoutes(groupRoute *gin.RouterGroup) {
	groupMcpRoute := groupRoute.Group("/:group/mcp")
	{
		groupMcpRoute.GET("/", mcp.GetGroupPublicMCPs)
		groupMcpRoute.GET("/:id", mcp.GetGroupPublicMCPByID)
	}
}

func setMCPAPIRoutes(apiRouter *gin.RouterGroup) {
	publicsMcpRoute := apiRouter.Group("/mcp/publics")
	{
		publicsMcpRoute.GET("/", mcp.GetPublicMCPs)
		publicsMcpRoute.GET("/all", mcp.GetAllPublicMCPs)
		publicsMcpRoute.POST("/", mcp.SavePublicMCPs)
	}

	publicMcpRoute := apiRouter.Group("/mcp/public")
	{
		publicMcpRoute.GET("/:id", mcp.GetPublicMCPByID)
		publicMcpRoute.POST("/", mcp.CreatePublicMCP)
		publicMcpRoute.POST("/:id", mcp.UpdatePublicMCP)
		publicMcpRoute.PUT("/:id", mcp.SavePublicMCP)
		publicMcpRoute.DELETE("/:id", mcp.DeletePublicMCP)
		publicMcpRoute.POST("/:id/status", mcp.UpdatePublicMCPStatus)
		publicMcpRoute.GET("/:id/group/:group/params", mcp.GetGroupPublicMCPReusingParam)
		publicMcpRoute.POST(
			"/:id/group/:group/params",
			mcp.SaveGroupPublicMCPReusingParam,
		)
	}

	groupMcpRoute := apiRouter.Group("/mcp/group")
	{
		groupMcpRoute.GET("/:group", mcp.GetGroupMCPs)
		groupMcpRoute.GET("/all", mcp.GetAllGroupMCPs)
		groupMcpRoute.GET("/:group/:id", mcp.GetGroupMCPByID)
		groupMcpRoute.POST("/:group", mcp.CreateGroupMCP)
		groupMcpRoute.PUT("/:group/:id", mcp.UpdateGroupMCP)
		groupMcpRoute.DELETE("/:group/:id", mcp.DeleteGroupMCP)
		groupMcpRoute.POST("/:group/:id/status", mcp.UpdateGroupMCPStatus)
	}

	embedMcpRoute := apiRouter.Group("/embedmcp")
	{
		embedMcpRoute.GET("/", mcp.GetEmbedMCPs)
		embedMcpRoute.POST("/", mcp.SaveEmbedMCP)
	}

	testEmbedMcpRoute := apiRouter.Group("/test-embedmcp")
	{
		testEmbedMcpRoute.GET("/:id/sse", mcp.TestEmbedMCPSseServer)
		testEmbedMcpRoute.GET("/:id", mcp.TestEmbedMCPStreamable)
		testEmbedMcpRoute.POST("/:id", mcp.TestEmbedMCPStreamable)
		testEmbedMcpRoute.DELETE("/:id", mcp.TestEmbedMCPStreamable)
	}

	testPublicMcpRoute := apiRouter.Group("/test-publicmcp")
	{
		testPublicMcpRoute.GET("/:group/:id/sse", mcp.TestPublicMCPSSEServer)
	}
}
