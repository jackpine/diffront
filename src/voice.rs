use accesskit::{
    Action, ActionHandler, ActionRequest, ActivationHandler, Node, NodeId, Rect, Role, TreeId,
    TreeInfo, TreeUpdate,
};
use accesskit_macos::SubclassingAdapter;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

const ROOT: NodeId = NodeId(0);
const BUTTON_WIDTH: f64 = 130.;
const BUTTON_HEIGHT: f64 = 24.;
const BUTTON_MARGIN: f64 = 8.;

/// An action Voice Control can invoke by name.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VoiceCommand {
    MarkReviewed,
    PageDown,
    PageUp,
}

impl VoiceCommand {
    const ALL: [Self; 3] = [Self::MarkReviewed, Self::PageDown, Self::PageUp];

    fn label(self) -> &'static str {
        match self {
            Self::MarkReviewed => "Mark as reviewed",
            Self::PageDown => "Page down",
            Self::PageUp => "Page up",
        }
    }

    fn node_id(self) -> NodeId {
        let ix = Self::ALL.iter().position(|command| *command == self);
        NodeId(ix.expect("ALL lists every command") as u64 + 1)
    }

    fn from_node_id(id: NodeId) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|command| command.node_id() == id)
    }

    /// A button in the window's top left, sized in physical pixels as AccessKit expects.
    fn node(self, scale: f64) -> Node {
        let ix = Self::ALL.iter().position(|command| *command == self);
        let x = BUTTON_MARGIN
            + ix.expect("ALL lists every command") as f64 * (BUTTON_WIDTH + BUTTON_MARGIN);
        let mut node = Node::new(Role::Button);
        node.set_label(self.label());
        node.add_action(Action::Click);
        node.set_bounds(Rect {
            x0: x * scale,
            y0: BUTTON_MARGIN * scale,
            x1: (x + BUTTON_WIDTH) * scale,
            y1: (BUTTON_MARGIN + BUTTON_HEIGHT) * scale,
        });
        node
    }
}

struct InitialTree {
    scale: f64,
}

impl ActivationHandler for InitialTree {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        let mut root = Node::new(Role::Window);
        root.set_label("diffront");
        root.set_children(VoiceCommand::ALL.map(VoiceCommand::node_id).to_vec());
        let mut nodes = vec![(ROOT, root)];
        nodes
            .extend(VoiceCommand::ALL.map(|command| (command.node_id(), command.node(self.scale))));
        Some(TreeUpdate {
            nodes,
            tree: Some(TreeInfo::new(ROOT)),
            tree_id: TreeId::ROOT,
            focus: ROOT,
        })
    }
}

struct CommandSender(UnboundedSender<VoiceCommand>);

impl ActionHandler for CommandSender {
    fn do_action(&mut self, request: ActionRequest) {
        if request.action == Action::Click
            && let Some(command) = VoiceCommand::from_node_id(request.target_node)
        {
            self.0.unbounded_send(command).ok();
        }
    }
}

/// Publishes the commands to the window's accessibility tree; they arrive on the receiver.
pub fn publish(window: &Window) -> (SubclassingAdapter, UnboundedReceiver<VoiceCommand>) {
    let handle = HasWindowHandle::window_handle(window).expect("window has a native handle");
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        unreachable!("macOS windows have AppKit handles");
    };
    let (sender, receiver) = unbounded();
    let initial_tree = InitialTree {
        scale: window.scale_factor().into(),
    };
    // SAFETY: the view outlives this call, and the adapter retains it.
    let adapter = unsafe {
        SubclassingAdapter::new(handle.ns_view.as_ptr(), initial_tree, CommandSender(sender))
    };
    (adapter, receiver)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicks_on_buttons_send_their_command() {
        let (sender, mut receiver) = unbounded();
        let mut handler = CommandSender(sender);
        let click = |target_node, action| ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node,
            data: None,
        };
        handler.do_action(click(VoiceCommand::PageUp.node_id(), Action::Click));
        handler.do_action(click(VoiceCommand::PageUp.node_id(), Action::Focus));
        handler.do_action(click(ROOT, Action::Click));
        assert_eq!(receiver.try_recv().ok(), Some(VoiceCommand::PageUp));
        assert!(receiver.try_recv().is_err());
    }
}
