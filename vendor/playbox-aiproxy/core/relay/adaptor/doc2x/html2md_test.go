package doc2x_test

import (
	"strings"
	"testing"

	"github.com/labring/aiproxy/core/relay/adaptor/doc2x"
)

func TestCleanMarkdownNormalizesDoc2XFragments(t *testing.T) {
	t.Parallel()

	input := `<!-- Meanless: 人工智能生成内容（AIGC）白皮书 -->
深度神经网络在学习范式 \( {}^{2} \) 和矩阵 \[ x+y \]
<!-- Media -->
<!-- figureText: 输入图像。<br>输出结果 -->
<!-- Footnote -->`
	result := doc2x.CleanMarkdown(input)

	if !strings.Contains(result, `深度神经网络在学习范式 [^2] 和矩阵 $$ x+y $$`) {
		t.Fatalf("math delimiters or footnote reference were not normalized: %q", result)
	}

	if !strings.Contains(result, "**图内文字**\n\n输入图像。\n输出结果") {
		t.Fatalf("figure text was lost: %q", result)
	}

	for _, marker := range []string{"<!-- Meanless:", "<!-- Media -->", "<!-- figureText:", "<!-- Footnote -->"} {
		if strings.Contains(result, marker) {
			t.Fatalf("comment marker %q remains in %q", marker, result)
		}
	}
}

func TestCleanMarkdownNormalizesMathTagsAndTextSubscripts(t *testing.T) {
	t.Parallel()

	result := doc2x.CleanMarkdown(`$x + y \tag{1}$ and \text{where a_b is defined}`)

	expected := `$$x + y \qquad \qquad (1)$$ and \text{where a\_b is defined}`
	if result != expected {
		t.Fatalf("expected %q, got %q", expected, result)
	}
}

func TestCleanMarkdownFormatsFootnoteBlock(t *testing.T) {
	t.Parallel()

	input := `正文 \( {}^{2} \)

---
<!-- Footnote -->
2 脚注说明

https://example.com/reference
<!-- Footnote -->
---`
	result := doc2x.CleanMarkdown(input)

	expected := "正文 [^2]\n\n[^2]: 脚注说明\n\n    https://example.com/reference"
	if result != expected {
		t.Fatalf("expected %q, got %q", expected, result)
	}

	if strings.Contains(result, "<!-- Footnote -->") || strings.Contains(result, "\n---\n") {
		t.Fatalf("footnote markers or separators remain: %q", result)
	}
}

func TestCleanMarkdownCollapsesBlankLinesWithoutDroppingText(t *testing.T) {
	t.Parallel()

	result := doc2x.CleanMarkdown("第一段\n\n\n\n第二段")
	if result != "第一段\n\n第二段" {
		t.Fatalf("expected normalized paragraphs, got %q", result)
	}
}

func TestHTMLImage2MdPreservesSignedURLQuery(t *testing.T) {
	t.Parallel()

	input := `<img src="https://img.doc2x.noedgeai.com/page.jpg?x=275&y=216&w=1189&h=678&r=0"/>`

	expected := `![img](https://img.doc2x.noedgeai.com/page.jpg?x=275&y=216&w=1189&h=678&r=0)`
	if result := doc2x.HTMLImage2Md(input); result != expected {
		t.Fatalf("expected %q, got %q", expected, result)
	}
}

func TestHTMLTable2Md(t *testing.T) {
	t.Parallel()

	tables := []struct {
		name     string
		html     string
		expected string
	}{
		{
			name: "basic table",
			html: `<table><tr><td>sadsa</td><td/><td/></tr><tr><td/><td>sadasdsa</td><td>sad</td></tr><tr><td/><td/><td>dsadsadsa</td></tr><tr><td/><td/><td/></tr></table>`,
			expected: `| sadsa |  |  |
| --- | --- | --- |
|  | sadasdsa | sad |
|  |  | dsadsadsa |
|  |  |  |`,
		},
		{
			name: "simple table",
			html: `<table><tr><td>Header 1</td><td>Header 2</td></tr><tr><td>Data 1</td><td>Data 2</td></tr></table>`,
			expected: `| Header 1 | Header 2 |
| --- | --- |
| Data 1 | Data 2 |`,
		},
		{
			name: "empty table",
			html: `<table><tr><td></td><td></td></tr><tr><td></td><td></td></tr></table>`,
			expected: `|  |  |
| --- | --- |
|  |  |`,
		},
	}

	for _, tc := range tables {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			result := doc2x.HTMLTable2Md(tc.html)

			if result != tc.expected {
				t.Errorf("Expected:\n%s\nGot:\n%s", tc.expected, result)
			}
		})
	}
}

// var htmlImage = `<img
// src="https://cdn.noedgeai.com/01956426-b164-730d-a1fe-8be8972145d6_0.jpg?x=258&y=694&w=1132&h=826"/>`

// func TestInlineMdImage(t *testing.T) {
// 	t.Parallel()
// 	result := doc2x.InlineMdImage(context.Background(), htmlImage)
// 	t.Log(result)
// }
