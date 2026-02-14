use lapce_xi_rope::{Rope, rope::ChunkIter};
use tree_sitter::TextProvider;

pub struct RopeChunksIterBytes<'a> {
    chunks: ChunkIter<'a>,
}
impl<'a> Iterator for RopeChunksIterBytes<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        self.chunks.next().map(str::as_bytes)
    }
}

/// Adapter that lets tree-sitter read directly from a Rope's internal chunks
/// without materializing the entire text into a single contiguous buffer.
/// This is critical for performance on large files since Ropes store text
/// in a balanced tree of small string chunks.
pub struct RopeProvider<'a>(pub &'a Rope);
impl<'a> TextProvider<&'a [u8]> for RopeProvider<'a> {
    type I = RopeChunksIterBytes<'a>;

    fn text(&mut self, node: tree_sitter::Node) -> Self::I {
        let start = node.start_byte();
        let end = node.end_byte().min(self.0.len());
        let chunks = self.0.iter_chunks(start..end);
        RopeChunksIterBytes { chunks }
    }
}
