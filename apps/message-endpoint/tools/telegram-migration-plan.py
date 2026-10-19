#!/usr/bin/env python3
"""Offline read-only inventory. Never calls Treer/Telegram or activates anything."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import stat

MAX_ROWS = 1000
MAX_ID = 2**63 - 2

class Invalid(Exception):
    pass

def integer(value, minimum=0):
    if type(value) is not int or not minimum <= value <= MAX_ID:
        raise Invalid('invalid_numeric_metadata')
    return value

def fixed_id(value, prefix=''):
    if not isinstance(value, str) or not re.fullmatch(re.escape(prefix)+r'[A-Za-z0-9_-]{1,128}', value):
        raise Invalid('invalid_identifier_metadata')
    return value

def private_source(path):
    path = Path(os.path.abspath(path))
    for part in [path, *path.parents]:
        if part.is_symlink():
            raise Invalid('symlink_source')
    info = path.stat()
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise Invalid('source_not_private_regular_file')
    parent = path.parent.stat()
    if parent.st_uid != os.getuid() or parent.st_mode & 0o077:
        raise Invalid('source_directory_not_private')
    for suffix in ['-wal', '-shm', '-journal']:
        side = Path(str(path)+suffix)
        if side.exists() or side.is_symlink():
            # Never touch a production WAL/SHM, even SQLite reader marks. The
            # owner must first make a closed, consistent online-backup file.
            raise Invalid('closed_online_backup_required')
    return path, (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns)

def inventory(path):
    path, identity = private_source(path)
    db = sqlite3.connect(path.as_uri()+'?mode=ro&immutable=1', uri=True, timeout=2)
    try:
        db.execute('PRAGMA query_only=ON')
        db.execute('BEGIN')  # closed immutable snapshot; no WAL/SHM access
        ticks = 0
        def budget():
            nonlocal ticks
            ticks += 1
            return ticks > 10000
        db.set_progress_handler(budget, 1000)
        def rows(sql, parameters=()):
            result = db.execute(sql, parameters).fetchmany(MAX_ROWS+1)
            if len(result) > MAX_ROWS:
                raise Invalid('inventory_capacity_exceeded')
            return result
        metadata = dict(rows("SELECT key,value FROM metadata WHERE key IN ('bot_id','last_update_id')"))
        bot = metadata.get('bot_id')
        offset = metadata.get('last_update_id')
        if not isinstance(bot, str) or not re.fullmatch(r'[1-9][0-9]{0,18}', bot) or int(bot) > MAX_ID:
            raise Invalid('missing_or_invalid_bot_pin')
        if offset is None:
            last = -1
        elif not isinstance(offset, str) or not re.fullmatch(r'-1|0|[1-9][0-9]{0,18}', offset):
            raise Invalid('invalid_offset')
        else:
            last = integer(int(offset), -1)
        bindings = []
        for channel, account, conversation, target, enabled in rows('SELECT channel,account_id,conversation,target_agent_id,enabled FROM poc_bindings ORDER BY channel,account_id,conversation'):
            if channel != 'telegram':
                raise Invalid('unsupported_legacy_channel')
            match = re.fullmatch(r'(-?[1-9][0-9]{0,18}):(0|[1-9][0-9]{0,18})', conversation or '')
            if not match or abs(int(match[1])) > MAX_ID or int(match[2]) > MAX_ID or enabled not in (0, 1):
                raise Invalid('invalid_binding_metadata')
            bindings.append({'legacy_account': fixed_id(account), 'chat_id': int(match[1]), 'topic_id': int(match[2]) or None, 'recipient_agent_id': fixed_id(target, 'ag_'), 'enabled': bool(enabled), 'new_endpoint_id': None, 'new_generation': None})
        senders = []
        for channel, account, user, enabled in rows('SELECT channel,account_id,user_id,enabled FROM im_users ORDER BY channel,account_id,user_id'):
            if channel != 'telegram' or not re.fullmatch(r'[1-9][0-9]{0,18}', user or '') or enabled not in (0, 1):
                raise Invalid('invalid_sender_metadata')
            senders.append({'legacy_account': fixed_id(account), 'sender_id': integer(int(user), 1), 'enabled': bool(enabled)})
        def counts(table, allowed):
            result = dict(rows('SELECT status,count(*) FROM '+table+' GROUP BY status'))
            if set(result)-set(allowed):
                raise Invalid('unknown_legacy_delivery_status')
            return {key: integer(result.get(key, 0)) for key in allowed}
        outbound = counts('outbound_deliveries', ['pending','sending','error','sent','acked'])
        chunks = counts('outbound_chunks', ['pending','sending','error','sent'])
        processed = counts('processed_updates', ['completed'])
        mappings = integer(db.execute('SELECT count(*) FROM message_mappings').fetchone()[0])
        report = {'schema':'treer.telegram-migration-plan/v1','ready_for_cutover':False,'bot_id':int(bot),'legacy_last_update_id':last,'proposed_next_offset':last+1,'bindings':bindings,'senders':senders,'outbound_deliveries':outbound,'outbound_chunks':chunks,'processed_updates':processed,'historical_mapping_count':mappings,'blockers':['operator_evidence_not_verified','endpoint_mapping_required','offline_import_and_reverse_journal_not_implemented','core_mailbox_drain_not_verified']}
        if outbound['pending'] or outbound['sending'] or outbound['error'] or outbound['sent'] or chunks['pending'] or chunks['sending'] or chunks['error']:
            report['blockers'].append('legacy_outbound_not_drained')
        if outbound['sending'] or chunks['sending']:
            report['blockers'].append('ambiguous_inflight_send_requires_review')
        if mappings:
            report['blockers'].append('historical_context_mapping_requires_review')
        report['inventory_sha256'] = hashlib.sha256(json.dumps(report, sort_keys=True, separators=(',',':')).encode()).hexdigest()
        current = path.stat()
        if (current.st_dev,current.st_ino,current.st_size,current.st_mtime_ns) != identity:
            raise Invalid('source_changed')
        return report
    finally:
        db.close()

def write_private(path, report):
    path = Path(os.path.abspath(path))
    parent = path.parent
    for ancestor in Path(__file__).resolve().parents:
        if (ancestor/'.git').exists() and path.is_relative_to(ancestor):
            raise Invalid('report_must_be_outside_repository')
    for part in [parent, *parent.parents]:
        if part.is_symlink():
            raise Invalid('symlink_output_parent')
        if any((part/marker).exists() for marker in ['.git','.hg','.svn']):
            raise Invalid('report_must_be_outside_repository')
    s = parent.stat()
    if s.st_uid != os.getuid() or s.st_mode & 0o077:
        raise Invalid('output_directory_not_private')
    fd = os.open(path, os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd,'w') as stream:
        json.dump(report, stream, indent=2);stream.write('\n');stream.flush();os.fsync(stream.fileno())

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--legacy-db', required=True)
    parser.add_argument('--output', required=True, help='new file in an existing private directory; never overwritten')
    args = parser.parse_args()
    try:
        report = inventory(args.legacy_db)
        write_private(args.output, report)
    except (Invalid, OSError, sqlite3.Error, ValueError, TypeError):
        # Never print source values, filenames, SQL, DB errors or credential data.
        print('migration_preflight_failed', file=__import__('sys').stderr)
        return 1
    print(json.dumps({'ready_for_cutover':False,'blocker_count':len(report['blockers']),'inventory_sha256':report['inventory_sha256']}))
    return 0

if __name__ == '__main__':
    raise SystemExit(main())
