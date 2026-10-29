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

    def test_admin_actions_are_default_deny_for_agents_in_the_simulator(self):
        def run(document, mode, subject):
            bundle={'revision':1,'mode':mode,'document':{'schema_version':1,'groups':{},'defaults':{},'rules':[],**document}}
            return policy.evaluate_request({'subject':subject,'action':'org.invite.manage','resource':{'kind':'organization','id':'org_1'}},bundle)
        agent={'kind':'agent','id':'agent-a'}
        # No rules (monitor) and an allow default both deny Agents.
        self.assertEqual(run({}, 'monitor', agent)['decision'], 'deny')
        self.assertEqual(run({'defaults':{'org.invite.manage':'allow'}}, 'monitor', agent)['decision'], 'deny')
        rule=lambda subject:{'rules':[{'id':'r','priority':1,'effect':'allow','subjects':[subject],'actions':['org.invite.manage'],'resources':[{}]}]}
        # Selecting every Agent by kind does not name this one.
        self.assertEqual(run(rule({'kind':'agent'}), 'enforce', agent)['decision'], 'deny')
        for mode in ('monitor','enforce'):
            self.assertEqual(run(rule({'kind':'agent','id':'agent-a'}), mode, agent)['decision'], 'allow')
        # Humans keep role checks; the simulator does not apply the Agent gate.
        self.assertEqual(run({}, 'monitor', {'kind':'human','id':'usr_1'})['decision'], 'allow')
        self.assertEqual(set(policy.ADMIN_ACTIONS), {'org.manage','org.member.manage','org.invite.manage','org.group.manage','workspace.manage','workspace.access.manage','policy.provider.manage','audit.read','machine.enroll'})

    def test_simulator_naming_matches_core_exactly(self):
        # Same semantics as Core's selector_names_subject: only a non-empty id
        # or group names the subject; empty selectors and machine never do.
        agent={'kind':'agent','id':'agent-a','machine_id':'m'}
        def run(subject, action='org.manage', groups=None):
            bundle={'revision':1,'mode':'enforce','document':{'schema_version':1,'groups':groups or {},'defaults':{},
                'rules':[{'id':'r','priority':1,'effect':'allow','subjects':[subject],'actions':[action],'resources':[{}]}]}}
            return policy.evaluate_request({'subject':agent,'action':action,'resource':{'kind':'organization','id':'org_1'}},bundle)['decision']
        self.assertEqual(run({}), 'deny')
        self.assertEqual(run({'kind':'agent','machine_id':'m'}), 'deny')
        self.assertEqual(run({'id':'agent-a'}), 'allow')
        self.assertEqual(run({'group':'admins'}, groups={'admins':{'principals':[{'kind':'agent','id':'agent-a'}]}}), 'allow')
        self.assertEqual(run({'group':'admins'}, groups={'admins':{'principals':[{'kind':'agent','id':'other'}]}}), 'deny')
        # policy.provider.manage is never granted to non-humans.
        self.assertEqual(run({'id':'agent-a'}, action='policy.provider.manage'), 'deny')
        self.assertFalse(policy.selector_names_subject({'tree':{'path_eq':'/admins/a'}}))
        self.assertFalse(policy.selector_names_subject({'id':'', 'group':''}))

if __name__ == '__main__': unittest.main()
