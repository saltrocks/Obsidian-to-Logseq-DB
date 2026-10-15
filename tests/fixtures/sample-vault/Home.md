---
aliases:
  - Start here
tags: [hub, area/work]
rating: 5
done: true
website: https://obsidian.md
related: ["[[Project Alpha]]", "[[Missing Note]]"]
due: 2024-03-01
cssclasses: [wide]
---
# Welcome

Intro paragraph with **bold**, ==highlight==, `inline [[not a link]] #notag` and a #inline-tag.
See [[Project Alpha|the alpha project]], [[Project Alpha#Goals]], [[Project Alpha#^goal-1]] and [[Missing Note]].
Markdown link to [alpha](Projects/Alpha/Project%20Alpha.md) and [external](https://example.com#frag).
Issue #42 is not a tag. %%hidden comment%%

## Lists
- Parent item
	- Child item with [[2024-01-15]]
		- [ ] Open task ⏫ 📅 2024-02-01
	- [x] Finished task
	- [/] In progress
	- [-] Cancelled
1. First
2. Second

## Media
![[diagram.png|300]]
Inline image ![[diagram.png]] and ![md image](attachments/diagram.png) and [[spec.pdf]].

![](https://www.youtube.com/watch?v=1adNxqAtYMQ)

Remote image ![logo](https://example.com/logo.png) stays.

![[Project Alpha]]

![[Project Alpha#Goals]]

> [!warning] Be careful
> This is a **callout** with [[Index]].

> [!info]- Folded info
> Body text

> A plain quote
> spanning lines

> [!quote] Ada Lovelace
> Quoted callout body

```rust
fn main() { println!("#not-a-tag [[nope]]"); }
```

$$
E = mc^2
$$

| Col | Link |
|-----|------|
| a   | [[Project Alpha\|alpha]] |

---
Last line ^home-end
