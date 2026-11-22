// The pinned Typst compiler renders the reference directly from content.typ.
#import "content.typ": manuscript
#set document(title: "Vesper language and platform specification")
#set raw(theme: none)
#show heading: it => {
  let attrs = if it.has("label") { (id: str(it.label)) } else { (:) }
  html.elem("h" + str(it.level + 1), attrs: attrs, it.body)
}
#show raw.where(block: true): it => html.elem(
  "pre",
  attrs: (tabindex: "0"),
  html.elem("code", attrs: (data-lang: if it.lang == none { "text" } else { it.lang }), it.text),
)
#show grid: it => html.frame(it)
#show math.overline: it => html.elem("mover", attrs: (accent: "true"), [
  #it.body
  #html.elem("mo", "¯")
])
#let infer-rule(name, premises, conclusion) = html.elem("figure", attrs: (class: "infer-rule"), [
  #html.elem("figcaption", name)
  #html.elem("div", premises)
  #html.elem("div", conclusion)
])
#manuscript(body => emph(body), infer: infer-rule)
