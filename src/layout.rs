use crate::{AlignItems, AlignSelf, FlexDirection, JustifyContent, Node, Padding, Position, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

pub(crate) struct PositionedNode<'a> {
    pub node: &'a Node,
    pub rect: Rect,
}

pub(crate) fn calculate(tree: &[Node], viewport: Rect) -> Vec<PositionedNode<'_>> {
    let mut nodes = Vec::new();
    for node in tree {
        layout(node, root_rect(node, viewport), &mut nodes);
    }
    nodes
}

fn root_rect(node: &Node, viewport: Rect) -> Rect {
    let style = node.style();
    let x = style.x.min(viewport.w);
    let y = style.y.min(viewport.h);
    Rect {
        x: viewport.x.saturating_add(x),
        y: viewport.y.saturating_add(y),
        w: style.width.unwrap_or(viewport.w - x).min(viewport.w - x),
        h: style.height.unwrap_or(viewport.h - y).min(viewport.h - y),
    }
}

fn layout<'a>(node: &'a Node, viewport: Rect, nodes: &mut Vec<PositionedNode<'a>>) {
    nodes.push(PositionedNode {
        node,
        rect: viewport,
    });
    let Node::Container(container) = node else {
        return;
    };
    let style = &container.style;
    let border = u16::from(style.border.is_some());
    let content = content_area(viewport, border, style.padding);
    let count = container.items.len();
    if count == 0 {
        return;
    }

    let relative: Vec<usize> = container
        .items
        .iter()
        .enumerate()
        .filter_map(|(index, child)| {
            (child.style().position == Position::Relative).then_some(index)
        })
        .collect();
    let mut child_rects = vec![None; count];
    let main_size = match style.flex_direction {
        FlexDirection::Row | FlexDirection::RowReverse => content.w,
        FlexDirection::Column | FlexDirection::ColumnReverse => content.h,
    };
    let gaps = u64::from(style.gap) * relative.len().saturating_sub(1) as u64;
    let available = u64::from(main_size).saturating_sub(gaps);
    let mut sizes = vec![0_u16; count];
    let mut fixed = 0_u64;
    let mut automatic = Vec::new();

    for &index in &relative {
        let child = container.items[index].style();
        let requested = match style.flex_direction {
            FlexDirection::Row | FlexDirection::RowReverse => child.width,
            FlexDirection::Column | FlexDirection::ColumnReverse => child.height,
        };
        if let Some(size) = requested {
            sizes[index] = size.min(main_size);
            fixed += u64::from(sizes[index]);
        } else {
            automatic.push(index);
        }
    }

    if !automatic.is_empty() {
        let space = available.saturating_sub(fixed);
        let base = space / automatic.len() as u64;
        let remainder = space % automatic.len() as u64;
        for (position, &index) in automatic.iter().enumerate() {
            sizes[index] = (base + u64::from((position as u64) < remainder)) as u16;
        }
    }

    let used = relative
        .iter()
        .fold(gaps, |total, &index| total + u64::from(sizes[index]));
    let free = u64::from(main_size).saturating_sub(used) as u16;
    let reversed = matches!(
        style.flex_direction,
        FlexDirection::RowReverse | FlexDirection::ColumnReverse
    );
    let mut cursor = match (style.justify_content, reversed) {
        (JustifyContent::Start, false) | (JustifyContent::End, true) => 0,
        (JustifyContent::End, false) | (JustifyContent::Start, true) => free,
        (JustifyContent::Center, _) => free / 2,
    };
    let mut order = relative;
    if reversed {
        order.reverse()
    }

    for index in order {
        let child_style = container.items[index].style();
        let main = sizes[index].min(main_size.saturating_sub(cursor));
        let (cross, cross_offset) = cross_layout(style, child_style, content);
        let rect = match style.flex_direction {
            FlexDirection::Column | FlexDirection::ColumnReverse => Rect {
                x: content.x + cross_offset,
                y: content.y + cursor,
                w: cross,
                h: main,
            },
            FlexDirection::Row | FlexDirection::RowReverse => Rect {
                x: content.x + cursor,
                y: content.y + cross_offset,
                w: main,
                h: cross,
            },
        };
        child_rects[index] = Some(offset_and_clip(rect, content, child_style.x, child_style.y));
        cursor = cursor.saturating_add(main).saturating_add(style.gap);
    }

    for (index, child) in container.items.iter().enumerate() {
        if child.style().position == Position::Absolute {
            child_rects[index] = Some(root_rect(child, content));
        }
    }
    for (index, child) in container.items.iter().enumerate() {
        if let Some(rect) = child_rects[index] {
            layout(child, rect, nodes)
        }
    }
}

fn cross_layout(parent: &Style, child: &Style, content: Rect) -> (u16, u16) {
    let (available, requested) = match parent.flex_direction {
        FlexDirection::Row | FlexDirection::RowReverse => (content.h, child.height),
        FlexDirection::Column | FlexDirection::ColumnReverse => (content.w, child.width),
    };
    let size = requested.unwrap_or(available).min(available);
    let free = available - size;
    let offset = match child.align_self {
        Some(AlignSelf::Start | AlignSelf::Stretch) => 0,
        Some(AlignSelf::End) => free,
        Some(AlignSelf::Center) => free / 2,
        None => match parent.align_items {
            AlignItems::Start | AlignItems::Stretch => 0,
            AlignItems::End => free,
            AlignItems::Center => free / 2,
        },
    };
    (size, offset)
}

fn content_area(viewport: Rect, border: u16, padding: Padding) -> Rect {
    let (top, right, bottom, left) = match padding {
        Padding::All(v) => (v, v, v, v),
        Padding::Top(v) => (v, 0, 0, 0),
        Padding::Bottom(v) => (0, 0, v, 0),
        Padding::Right(v) => (0, v, 0, 0),
        Padding::Left(v) => (0, 0, 0, v),
        Padding::Horizontal(v) => (0, v, 0, v),
        Padding::Vertical(v) => (v, 0, v, 0),
    };
    inset(
        viewport,
        border + u16::from(top),
        border + u16::from(right),
        border + u16::from(bottom),
        border + u16::from(left),
    )
}

fn inset(viewport: Rect, top: u16, right: u16, bottom: u16, left: u16) -> Rect {
    let left = left.min(viewport.w);
    let width = viewport.w - left;
    let right = right.min(width);
    let top = top.min(viewport.h);
    let height = viewport.h - top;
    let bottom = bottom.min(height);
    Rect {
        x: viewport.x + left,
        y: viewport.y + top,
        w: width - right,
        h: height - bottom,
    }
}

fn offset_and_clip(mut rect: Rect, bounds: Rect, x: u16, y: u16) -> Rect {
    let right = bounds.x.saturating_add(bounds.w);
    let bottom = bounds.y.saturating_add(bounds.h);
    rect.x = rect.x.saturating_add(x.min(right.saturating_sub(rect.x)));
    rect.y = rect.y.saturating_add(y.min(bottom.saturating_sub(rect.y)));
    rect.w = rect.w.min(right.saturating_sub(rect.x));
    rect.h = rect.h.min(bottom.saturating_sub(rect.y));
    rect
}
