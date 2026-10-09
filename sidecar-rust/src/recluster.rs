// Speaker clustering for diarization.
//
// sherpa-onnx's own clustering is not used for the speakers: asked for N
// clusters it returns fewer, and by threshold it merges two similar voices
// (two men in an interview) into one — after which their turns, glued
// together across the short pause between them, cannot be told apart
// again. Its segmentation is good, though. So the diarization child cuts
// sherpa's turns further at pauses, embeds every piece itself, and this
// module groups the pieces: agglomerative clustering by cosine similarity of
// duration-weighted embedding centroids, down to N speakers when the user
// gave a count, or until no two clusters sound alike in "auto" mode.
//
// Tiny clusters — a cough, a laugh, a one-word reply too short for a
// reliable embedding — would otherwise stand as speakers of their own (or,
// with an exact count, take the place of a real one). They are folded into
// the speaker they sound most like; "tiny" scales with the recording, so a
// 20-second clip keeps its short speakers.

/// How many speakers to end up with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeakerCount {
    /// Decide from the voices.
    Auto,
    /// Exactly this many, if there are that many distinguishable pieces.
    Exact(usize),
}

/// One stretch of a single speaker's speech.
#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    pub start: f64,
    pub end: f64,
    /// Sum of the unit-length embeddings of the piece's chunks, each scaled
    /// by its duration. `None` when the piece had no audio of its own to
    /// embed (shorter than the model needs, or all crosstalk).
    pub voice: Option<Vec<f32>>,
}

/// In "auto" mode two clusters are one speaker while the cosine similarity
/// of their centroids is at least this. Measured with the CAM++ embedding
/// on the synthetic corpus: merges of two halves of one voice scored 0.74
/// and up, two different voices 0.52 and below — except one pair of similar
/// female voices at 0.79, which "auto" cannot separate (an exact count can).
pub const AUTO_THRESHOLD: f32 = 0.6;

/// A cluster with less talk time than this (seconds) is "tiny": 2% of all
/// speech, at least 1 s and at most 8 s.
pub fn tiny_limit(total_talk: f64) -> f64 {
    (0.02 * total_talk).clamp(1.0, 8.0)
}

/// Groups `pieces` into speakers. Returns one label per piece; labels are
/// numbered 0, 1, … by first appearance in `pieces` order. A piece without
/// a voice joins the speaker of the closest piece in time.
pub fn cluster(pieces: &[Piece], count: SpeakerCount) -> Vec<usize> {
    let total: f64 = pieces.iter().map(|p| (p.end - p.start).max(0.0)).sum();
    let mut ahc = Ahc::new(pieces, tiny_limit(total));
    ahc.run(count);
    let roots = ahc.labels();

    // Voiceless pieces take the label of the nearest voiced one.
    let voiced: Vec<usize> = (0..pieces.len()).filter(|&i| roots[i].is_some()).collect();
    let mut labels: Vec<usize> = Vec::with_capacity(pieces.len());
    for (i, p) in pieces.iter().enumerate() {
        let root = roots[i].or_else(|| {
            let gap = |j: usize| {
                (pieces[j].start - p.end)
                    .max(p.start - pieces[j].end)
                    .max(0.0)
            };
            voiced
                .iter()
                .copied()
                .min_by(|&a, &b| gap(a).total_cmp(&gap(b)))
                .and_then(|j| roots[j])
        });
        labels.push(root.unwrap_or(usize::MAX));
    }
    renumber(&labels)
}

/// Relabels 0, 1, … in order of first appearance.
fn renumber(labels: &[usize]) -> Vec<usize> {
    let mut seen: Vec<usize> = Vec::new();
    labels
        .iter()
        .map(|l| match seen.iter().position(|s| s == l) {
            Some(i) => i,
            None => {
                seen.push(*l);
                seen.len() - 1
            }
        })
        .collect()
}

/// A cluster's most similar partner and their similarity.
type Best = Option<(usize, f32)>;

/// Agglomerative clustering with centroid linkage. Each cluster keeps the
/// sum of its pieces' voice vectors; similarity is the cosine of two sums.
/// For speed every cluster caches its most similar partner overall and its
/// most similar tiny partner, so a merge only rescans the rows it affects.
struct Ahc {
    /// Index of the piece behind each node; nodes are the voiced pieces.
    piece_of: Vec<usize>,
    /// Unit-length centroid and raw sum per node.
    unit: Vec<Vec<f32>>,
    sum: Vec<Vec<f32>>,
    talk: Vec<f64>,
    alive: Vec<bool>,
    /// Union-find parent, to map pieces to their final cluster.
    parent: Vec<usize>,
    best_any: Vec<Best>,
    best_tiny: Vec<Best>,
    n_alive: usize,
    tiny: f64,
    n_pieces: usize,
}

impl Ahc {
    fn new(pieces: &[Piece], tiny: f64) -> Self {
        let mut s = Self {
            piece_of: Vec::new(),
            unit: Vec::new(),
            sum: Vec::new(),
            talk: Vec::new(),
            alive: Vec::new(),
            parent: Vec::new(),
            best_any: Vec::new(),
            best_tiny: Vec::new(),
            n_alive: 0,
            tiny,
            n_pieces: pieces.len(),
        };
        for (i, p) in pieces.iter().enumerate() {
            let Some(v) = p
                .voice
                .as_ref()
                .filter(|v| norm(v) > 0.0 && v.iter().all(|x| x.is_finite()))
            else {
                continue;
            };
            let node = s.sum.len();
            s.piece_of.push(i);
            s.unit.push(normalized(v));
            s.sum.push(v.clone());
            s.talk.push((p.end - p.start).max(0.0));
            s.alive.push(true);
            s.parent.push(node);
        }
        s.n_alive = s.sum.len();
        s.best_any = vec![None; s.n_alive];
        s.best_tiny = vec![None; s.n_alive];
        for i in 0..s.n_alive {
            s.rescan(i);
        }
        s
    }

    fn is_big(&self, i: usize) -> bool {
        self.talk[i] >= self.tiny
    }

    fn sim(&self, a: usize, b: usize) -> f32 {
        dot(&self.unit[a], &self.unit[b])
    }

    fn rescan(&mut self, i: usize) {
        let (mut any, mut tiny): (Best, Best) = (None, None);
        for j in 0..self.sum.len() {
            if j == i || !self.alive[j] {
                continue;
            }
            let s = self.sim(i, j);
            if any.is_none_or(|(_, b)| s > b) {
                any = Some((j, s));
            }
            if !self.is_big(j) && tiny.is_none_or(|(_, b)| s > b) {
                tiny = Some((j, s));
            }
        }
        self.best_any[i] = any;
        self.best_tiny[i] = tiny;
    }

    /// The most similar pair that may merge. With an exact count, once no
    /// more than that many big clusters are left, two big ones never merge:
    /// the remaining merges fold tiny clusters in.
    fn best_pair(&self, count: SpeakerCount) -> Option<(usize, usize, f32)> {
        let n_big = (0..self.sum.len())
            .filter(|&i| self.alive[i] && self.is_big(i))
            .count();
        let guard_big = matches!(count, SpeakerCount::Exact(n) if n_big <= n);
        let mut best: Option<(usize, usize, f32)> = None;
        for i in 0..self.sum.len() {
            if !self.alive[i] {
                continue;
            }
            let cand = if guard_big && self.is_big(i) {
                self.best_tiny[i]
            } else {
                self.best_any[i]
            };
            if let Some((j, s)) = cand {
                if best.is_none_or(|(_, _, b)| s > b) {
                    best = Some((i, j, s));
                }
            }
        }
        best
    }

    fn merge(&mut self, a: usize, b: usize) {
        let (sa, sb) = (
            std::mem::take(&mut self.sum[a]),
            std::mem::take(&mut self.sum[b]),
        );
        self.sum[a] = sa.iter().zip(&sb).map(|(x, y)| x + y).collect();
        self.unit[a] = normalized(&self.sum[a]);
        self.unit[b] = Vec::new();
        self.talk[a] += self.talk[b];
        self.alive[b] = false;
        self.parent[b] = a;
        self.n_alive -= 1;
        let a_tiny = !self.is_big(a);
        self.rescan(a);
        for k in 0..self.sum.len() {
            if k == a || !self.alive[k] {
                continue;
            }
            let stale = |c: Best| c.is_some_and(|(j, _)| j == a || j == b);
            if stale(self.best_any[k]) || stale(self.best_tiny[k]) {
                self.rescan(k);
                continue;
            }
            let s = self.sim(k, a);
            if self.best_any[k].is_none_or(|(_, x)| s > x) {
                self.best_any[k] = Some((a, s));
            }
            if a_tiny && self.best_tiny[k].is_none_or(|(_, x)| s > x) {
                self.best_tiny[k] = Some((a, s));
            }
        }
    }

    fn run(&mut self, count: SpeakerCount) {
        let floor = match count {
            SpeakerCount::Exact(n) => n.max(1),
            SpeakerCount::Auto => 1,
        };
        while self.n_alive > floor {
            let Some((a, b, s)) = self.best_pair(count) else {
                break;
            };
            if count == SpeakerCount::Auto && s < AUTO_THRESHOLD {
                break;
            }
            // Keep the bigger cluster's node, so a tiny one never absorbs.
            let (a, b) = if self.talk[a] >= self.talk[b] {
                (a, b)
            } else {
                (b, a)
            };
            self.merge(a, b);
        }
        if count == SpeakerCount::Auto {
            // Whatever tiny clusters are left sound like nobody in
            // particular; each joins the big cluster it is closest to.
            loop {
                let alive: Vec<usize> = (0..self.sum.len()).filter(|&i| self.alive[i]).collect();
                let Some(&t) = alive
                    .iter()
                    .filter(|&&i| !self.is_big(i))
                    .min_by(|&&x, &&y| self.talk[x].total_cmp(&self.talk[y]))
                else {
                    break;
                };
                let Some(&big) = alive
                    .iter()
                    .filter(|&&i| self.is_big(i))
                    .max_by(|&&x, &&y| self.sim(t, x).total_cmp(&self.sim(t, y)))
                else {
                    break;
                };
                self.merge(big, t);
            }
        }
    }

    fn root(&self, mut i: usize) -> usize {
        while self.parent[i] != i {
            i = self.parent[i];
        }
        i
    }

    /// Cluster (root node) per piece; `None` for pieces without a voice.
    fn labels(&self) -> Vec<Option<usize>> {
        let mut out = vec![None; self.n_pieces];
        for node in 0..self.piece_of.len() {
            out[self.piece_of[node]] = Some(self.root(node));
        }
        out
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn norm(v: &[f32]) -> f32 {
    dot(v, v).sqrt()
}

fn normalized(v: &[f32]) -> Vec<f32> {
    let n = norm(v);
    if n > 0.0 {
        v.iter().map(|x| x / n).collect()
    } else {
        v.to_vec()
    }
}

/// Unit-length `v` scaled by `weight`, to add into a piece's voice.
pub fn weighted_unit(v: &[f32], weight: f32) -> Vec<f32> {
    normalized(v).into_iter().map(|x| x * weight).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A voice: a unit vector along `axis` with a little of `blend` mixed
    /// in, in 8 dimensions.
    fn voice(axis: usize, blend: usize, amount: f32, secs: f64) -> Option<Vec<f32>> {
        let mut v = vec![0.0f32; 8];
        v[axis] = 1.0;
        v[blend] += amount;
        Some(weighted_unit(&v, secs as f32))
    }

    fn piece(start: f64, secs: f64, v: Option<Vec<f32>>) -> Piece {
        Piece {
            start,
            end: start + secs,
            voice: v,
        }
    }

    /// Three speakers on axes 0, 1, 2, each piece slightly different.
    fn meeting() -> Vec<Piece> {
        let mut out = Vec::new();
        let mut t = 0.0;
        for k in 0..12 {
            let spk = k % 3;
            out.push(piece(t, 5.0, voice(spk, 3 + k % 4, 0.2, 5.0)));
            t += 6.0;
        }
        out
    }

    #[test]
    fn auto_finds_the_speakers() {
        let labels = cluster(&meeting(), SpeakerCount::Auto);
        assert_eq!(labels, vec![0, 1, 2, 0, 1, 2, 0, 1, 2, 0, 1, 2]);
    }

    #[test]
    fn exact_count_merges_the_most_similar_voices() {
        let mut pieces = meeting();
        // Speaker 2 now sounds a lot like speaker 0.
        for (k, p) in pieces.iter_mut().enumerate() {
            if k % 3 == 2 {
                p.voice = voice(0, 2, 0.9, 5.0);
            }
        }
        assert_eq!(
            cluster(&pieces, SpeakerCount::Exact(2)),
            vec![0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1, 0]
        );
        // Asked for three it still tells them apart: different enough for
        // an exact count, not for "auto".
        assert_eq!(
            cluster(&pieces, SpeakerCount::Exact(3)),
            vec![0, 1, 2, 0, 1, 2, 0, 1, 2, 0, 1, 2]
        );
        assert_eq!(
            cluster(&pieces, SpeakerCount::Auto),
            vec![0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1, 0]
        );
        // More than there are pieces: every piece its own speaker.
        let two = &pieces[..2];
        assert_eq!(cluster(two, SpeakerCount::Exact(5)), vec![0, 1]);
    }

    #[test]
    fn tiny_outlier_does_not_take_a_speaker_slot() {
        // Two speakers plus a 0.5 s cough unlike anyone; asked for two.
        let mut pieces = Vec::new();
        for k in 0..6 {
            pieces.push(piece(k as f64 * 6.0, 5.0, voice(k % 2, 4, 0.1, 5.0)));
        }
        pieces.push(piece(40.0, 0.5, voice(7, 6, 0.0, 0.5)));
        let labels = cluster(&pieces, SpeakerCount::Exact(2));
        assert_eq!(&labels[..6], &[0, 1, 0, 1, 0, 1]);
        assert!(labels[6] < 2, "{labels:?}");
        // In auto mode it is folded in too.
        let labels = cluster(&pieces, SpeakerCount::Auto);
        assert_eq!(&labels[..6], &[0, 1, 0, 1, 0, 1]);
        assert!(labels[6] < 2);
    }

    #[test]
    fn short_recordings_keep_short_speakers() {
        // 12 s in all: a 2 s speaker is not tiny here (limit 1 s).
        let pieces = vec![
            piece(0.0, 5.0, voice(0, 4, 0.1, 5.0)),
            piece(5.0, 2.0, voice(1, 4, 0.1, 2.0)),
            piece(7.0, 5.0, voice(0, 5, 0.1, 5.0)),
        ];
        assert_eq!(cluster(&pieces, SpeakerCount::Auto), vec![0, 1, 0]);
        assert_eq!(tiny_limit(12.0), 1.0);
        assert_eq!(tiny_limit(200.0), 4.0);
        assert_eq!(tiny_limit(3600.0), 8.0);
    }

    #[test]
    fn one_voice_is_one_speaker() {
        let pieces: Vec<Piece> = (0..8)
            .map(|k| piece(k as f64 * 4.0, 3.0, voice(0, 1 + k % 5, 0.3, 3.0)))
            .collect();
        assert!(cluster(&pieces, SpeakerCount::Auto).iter().all(|&l| l == 0));
    }

    #[test]
    fn voiceless_pieces_join_their_neighbour() {
        let pieces = vec![
            piece(0.0, 5.0, voice(0, 4, 0.1, 5.0)),
            piece(5.2, 0.3, None),
            piece(6.0, 5.0, voice(1, 4, 0.1, 5.0)),
            piece(11.5, 0.2, None),
            piece(12.0, 5.0, voice(0, 4, 0.1, 5.0)),
        ];
        // 5.2–5.5 is closer to the first piece; 11.5–11.7 touches the last.
        assert_eq!(cluster(&pieces, SpeakerCount::Auto), vec![0, 0, 1, 0, 0]);
        // Nothing voiced at all: one speaker.
        let mute = vec![piece(0.0, 1.0, None), piece(2.0, 1.0, None)];
        assert_eq!(cluster(&mute, SpeakerCount::Auto), vec![0, 0]);
        assert!(cluster(&[], SpeakerCount::Exact(3)).is_empty());
    }

    #[test]
    fn broken_embeddings_are_treated_as_missing() {
        let pieces = vec![
            piece(0.0, 5.0, voice(0, 4, 0.1, 5.0)),
            piece(6.0, 5.0, Some(vec![f32::NAN; 8])),
            piece(12.0, 5.0, Some(vec![0.0; 8])),
        ];
        assert_eq!(cluster(&pieces, SpeakerCount::Exact(2)), vec![0, 0, 0]);
    }
}
