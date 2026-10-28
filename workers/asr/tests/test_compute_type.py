import ast
import pathlib
import unittest

U = chr(95) + chr(95)  # dunder prefix/suffix
HERE = pathlib.Path(globals()[U + 'file' + U]).resolve()
WORKER = HERE.parents[1] / 'faster_whisper_worker.py'


def load_function(name):
    # The worker runs main() at import time, so compile just the one function.
    tree = ast.parse(WORKER.read_text(encoding='utf-8'))
    node = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == name)
    namespace = {}
    exec(compile(ast.Module(body=[node], type_ignores=[]), str(WORKER), 'exec'), namespace)
    return namespace[name]


choose_compute_type = load_function('choose_compute_type')


class ChooseComputeTypeTest(unittest.TestCase):
    def test_cpu_is_int8(self):
        self.assertEqual(choose_compute_type('cpu', {'int8', 'float32'}), 'int8')

    def test_volta_and_newer_keep_int8_float16(self):
        # e.g. RTX 3050 Ti (sm_86)
        supported = {'float32', 'int8', 'int8_float32', 'float16', 'int8_float16', 'bfloat16'}
        self.assertEqual(choose_compute_type('cuda', supported), 'int8_float16')

    def test_pascal_falls_back_to_int8(self):
        # e.g. GTX 1070 (sm_61): no efficient float16 in ctranslate2
        supported = {'float32', 'int8', 'int8_float32'}
        self.assertEqual(choose_compute_type('cuda', supported), 'int8')

    def test_nothing_quantized_uses_float32(self):
        self.assertEqual(choose_compute_type('cuda', {'float32'}), 'float32')
