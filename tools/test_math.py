"""Optional math export contract; install requirements-math.txt to run."""
import importlib.util
import unittest
from latex2omml import latex_to_omml

@unittest.skipUnless(all(importlib.util.find_spec(x) for x in ('lxml','latex2mathml','mathml2omml')), 'optional math dependencies not installed')
class MathExport(unittest.TestCase):
    def test_fractions_scripts_roots_and_matrix_are_native_math(self):
        from lxml import etree
        namespace={'m':'http://schemas.openxmlformats.org/officeDocument/2006/math'}
        for latex,element in [(r'\frac{x^2}{3}','f'),(r'x^2','sSup'),(r'\sqrt{x}','rad'),(r'\begin{matrix}1&2\\3&4\end{matrix}','m')]:
            with self.subTest(latex=latex):
                root=etree.fromstring(latex_to_omml(latex).encode())
                self.assertEqual(etree.QName(root).localname,'oMathPara')
                self.assertTrue(root.xpath(f'.//m:{element}',namespaces=namespace))

if __name__ == '__main__': unittest.main()
