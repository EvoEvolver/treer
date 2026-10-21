"""Schema v2 and editable template contracts; no managed environment."""
import copy
import json
import re
import importlib.util
import unittest
from pathlib import Path
spec = importlib.util.spec_from_file_location('schema_policy', Path(__file__).resolve().parents[1] / 'policy.py')
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)

class SchemaTests(unittest.TestCase):
    def test_metrics_seed_humans_only_and_monitor_compatibility(self):
        action = policy.ACTION_MACHINE_METRICS_READ
        self.assertNotIn(action, policy.READ_ACTIONS)
        self.assertNotIn(action, policy.WRITE_ACTIONS)
        for template in policy.TEMPLATES:
            document = policy.template_document(template['id'], {})['document']
            # Only the metrics slice has v1 selectors. Exercise the production
            # evaluator, without pretending to evaluate the v2 tree rules here.
            metrics = {'schema_version': 1, 'groups': {},
                       'defaults': {action: document['defaults'][action]},
                       'rules': [r for r in document['rules'] if action in r['actions']]}
            self.assertEqual(len(metrics['rules']), 1)
            for mode in ('enforce', 'monitor'):
                for kind, identifier in (('human', 'owner'), ('human', 'member'),
                                         ('agent', 'worker'), ('machine', 'controller'),
                                         ('service', 'app')):
                    with self.subTest(template=template['id'], mode=mode, actor=identifier):
                        result = policy.evaluate_request({
                            'subject': {'kind': kind, 'id': identifier},
                            'action': action, 'resource': {'kind': 'machine', 'id': 'visible'}},
                            {'revision': 1, 'mode': mode, 'document': metrics})
                        self.assertEqual(result['effect'], 'allow' if kind == 'human' else 'deny')
                        self.assertEqual(result['decision'],
                                         'allow' if kind == 'human' or mode == 'monitor' else 'deny')
            wrong_resource = policy.evaluate_request({
                'subject': {'kind': 'human', 'id': 'owner'}, 'action': action,
                'resource': {'kind': 'agent', 'id': 'other'}},
                {'revision': 1, 'mode': 'enforce', 'document': metrics})
            self.assertEqual(wrong_resource['decision'], 'deny')

    def rule_document(self):
        return policy.template_document('team-isolation', {})['document']

    def test_readme_isolation_example_scope_and_priority(self):
        readme=(Path(__file__).resolve().parents[1]/'README.md').read_text()
        example=next(json.loads(block) for block in re.findall(r'```json\n(.*?)\n```', readme, re.S) if 'policy-admin-access' in block)
        policy.validate_document(example)
        # This example uses only literal path_eq, so replacing each exact path
        # with a stable test ID is equivalent to Core's exact tree match. Use
        # the production v1 evaluator for the same descending-priority semantics;
        # this does not emulate Core's full v2 engine or OS/network isolation.
        translated=copy.deepcopy(example);translated['schema_version']=1
        for rule in translated['rules']:
            for selector in rule['subjects'] + rule['resources']:
                tree=selector.pop('tree', None)
                if tree:
                    self.assertTrue(set(tree) <= {'path_eq','kind'})
                    selector['id']=tree['path_eq']
        bundle={'revision':1,'mode':'enforce','document':translated}
        for action in ('identity.token.issue','service.connect','network.connect'):
            for actor,expected in (('/policy-admin/maintainer','allow'),('/team/worker','deny')):
                result=policy.evaluate_request({'subject':{'kind':'agent','id':actor},'action':action,'resource':{'kind':'machine.service','id':'/policy-admin/policy'}},bundle)
                self.assertEqual(result['decision'],expected)
            result=policy.evaluate_request({'subject':{'kind':'agent','id':'/team/worker'},'action':action,'resource':{'kind':'machine.service','id':'/other/app'}},bundle)
            self.assertEqual(result['decision'],'allow','Example must not deny unrelated Apps')

    def test_templates_only_generate_valid_monitor_drafts(self):
        for template in policy.TEMPLATES:
            result = policy.template_document(template['id'], {'actor_path':'/team','target_path':'/'})
            self.assertEqual(result['mode'], 'monitor')
            self.assertTrue(result['warnings'])
            self.assertEqual(policy.validate_document(result['document']), result['document'])
        doc = self.rule_document()
        self.assertEqual(doc['defaults']['agent.prompt'], 'deny')
        self.assertEqual(doc['rules'][0]['conditions'], [{'op':'same_folder'}])
        self.assertEqual(doc['rules'][1]['conditions'], [{'op':'resource_below_subject_folder','strict':True}])

    def test_globs_are_component_bounded_and_canonical(self):
        for path in ('/', '/team/*', '/team/**', '/**', '/Team/中文/*'):
            self.assertEqual(policy.validate_path(path, True), path)
        for path in ('team', '/team/', '//team', '/a/../b', '/a/./b', '/a/**/b', '/a/**/**', '/re*', '/a/?', '/a/[x]', '/ team', '/team '):
            with self.subTest(path=path), self.assertRaises(policy.PolicyError): policy.validate_path(path, True)
        self.assertEqual(policy.validate_path('/literal/*'), '/literal/*')

    def test_unknown_fields_bindings_and_schema_one_rejected(self):
        baseline = self.rule_document()
        cases = []
        doc = copy.deepcopy(baseline);doc['schema_version'] = 1;cases.append(doc)
        doc = copy.deepcopy(baseline);doc['rules'][0]['subjects'][0]['tree']['path_glob'] = '/**';cases.append(doc)
        doc = copy.deepcopy(baseline);doc['rules'][0]['conditions'] = [{'op':'same_folder','strict':False}];cases.append(doc)
        doc = copy.deepcopy(baseline);doc['rules'][0]['bindings'] = {'a':{'var':'a'}};cases.append(doc)
        doc = copy.deepcopy(baseline);doc['rules'][0]['conditions'] = [{'op':'resource_below_folder','folder':{'var':'missing'}}];cases.append(doc)
        doc = copy.deepcopy(baseline);doc['rules'][0]['priority'] = True;cases.append(doc)
        doc = copy.deepcopy(baseline);doc['rules'][0]['subjects'][0]['group'] = '';cases.append(doc)
        doc = copy.deepcopy(baseline);doc['rules'][0]['subjects'][0]['group'] = [];cases.append(doc)
        doc = copy.deepcopy(baseline);doc['rules'][0]['conditions'] = [{'op':'same_folder'}] * 33;cases.append(doc)
        doc = copy.deepcopy(baseline);doc['rules'][0]['bindings'] = {str(i):{'path':'/'} for i in range(9)};cases.append(doc)
        for index,doc in enumerate(cases):
            with self.subTest(index=index), self.assertRaises(policy.PolicyError): policy.validate_document(doc)
        doc = copy.deepcopy(baseline)
        doc['rules'][0]['bindings'] = {'team':{'ref':'subject.tree.folder'}}
        doc['rules'][0]['conditions'] = [{'op':'resource_below_folder','folder':{'var':'team'},'strict':False}]
        self.assertEqual(policy.validate_document(doc), doc)

    def test_v2_cannot_use_local_v1_simulator(self):
        with self.assertRaises(policy.PolicyError) as caught:
            policy.evaluate_request({'subject':{},'action':'agent.prompt','resource':{}}, {'document':self.rule_document(),'mode':'monitor','revision':1})
        self.assertEqual(caught.exception.code, 'core_explain_required')

if __name__ == '__main__': unittest.main()
