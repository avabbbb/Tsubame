import ast
import math
import pathlib
import unittest
from types import SimpleNamespace as NS

U = chr(95) + chr(95)  # dunder prefix/suffix
HERE = pathlib.Path(globals()[U + 'file' + U]).resolve()
WORKER = HERE.parents[1] / 'faster_whisper_worker.py'


def load_function(name):
    # The worker runs main() at import time, so compile just the one function.
    tree = ast.parse(WORKER.read_text(encoding='utf-8'))
    node = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == name)
    namespace = {'math': math}
    exec(compile(ast.Module(body=[node], type_ignores=[]), str(WORKER), 'exec'), namespace)
    return namespace[name]


segment_confidence = load_function('segment_confidence')


class SegmentConfidenceTest(unittest.TestCase):
    def test_segments_sharing_a_window_get_distinct_confidence(self):
        # Same avg_logprob (one decoded window), different word probabilities.
        a = NS(avg_logprob=-0.242, words=[NS(probability=0.9), NS(probability=0.7)])
        b = NS(avg_logprob=-0.242, words=[NS(probability=0.4)])
        self.assertAlmostEqual(segment_confidence(a), 0.8)
        self.assertAlmostEqual(segment_confidence(b), 0.4)

    def test_falls_back_to_avg_logprob_without_words(self):
        self.assertAlmostEqual(segment_confidence(NS(avg_logprob=-0.5, words=None)), math.exp(-0.5))

    def test_none_when_nothing_is_known(self):
        self.assertIsNone(segment_confidence(NS(avg_logprob=None, words=[])))

    def test_clamped(self):
        self.assertEqual(segment_confidence(NS(avg_logprob=0.3, words=None)), 1.0)


if __name__ == '__main__':
    unittest.main()
