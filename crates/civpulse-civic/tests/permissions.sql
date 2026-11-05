-- Run only in a disposable database populated with the website's schema.
-- Everything, including the synthetic auth users, is rolled back.
BEGIN;
DO $$ BEGIN
 IF current_database() <> 'civic_api_permissions' THEN
  RAISE EXCEPTION 'Use the isolated civic_api_permissions test database';
 END IF;
END $$;
INSERT INTO auth.users(id,email) VALUES
 ('00000000-0000-4000-8000-000000000001','civic-admin@example.invalid'),
 ('00000000-0000-4000-8000-000000000002','civic-owner@example.invalid'),
 ('00000000-0000-4000-8000-000000000003','civic-other@example.invalid');
INSERT INTO public.admins(user_id) VALUES('00000000-0000-4000-8000-000000000001');
INSERT INTO public.organizations(id,slug,name) VALUES('00000000-0000-4000-8000-000000000010','civic-permission-test','Permission test');
INSERT INTO public.people(id,slug,full_name,visibility) VALUES
 ('00000000-0000-4000-8000-000000000011','civic-public-test','Public test','public'),
 ('00000000-0000-4000-8000-000000000012','civic-private-test','Private test','private');
SET LOCAL ROLE authenticated;
SELECT set_config('request.jwt.claim.sub','00000000-0000-4000-8000-000000000001',true);
DO $$ DECLARE rid uuid; BEGIN
 rid:=public.save_records_request(NULL,'{"title":"Owner contract","target_organization_id":"00000000-0000-4000-8000-000000000010","visibility":"private"}');
 PERFORM set_config('civic.test_request',rid::text,true);
 PERFORM public.transfer_records_request(rid,'00000000-0000-4000-8000-000000000002','Permission test');
END $$;
SELECT set_config('request.jwt.claim.sub','00000000-0000-4000-8000-000000000002',true);
DO $$ DECLARE n integer; result jsonb; BEGIN
 IF public.is_admin() THEN RAISE EXCEPTION 'Owner became admin'; END IF;
 IF (SELECT count(*) FROM public.people)<>1 THEN RAISE EXCEPTION 'Private person leaked'; END IF;
 UPDATE public.people SET full_name='Forbidden' WHERE id='00000000-0000-4000-8000-000000000011';
 GET DIAGNOSTICS n=ROW_COUNT; IF n<>0 THEN RAISE EXCEPTION 'Ordinary user changed civic record'; END IF;
 UPDATE public.profiles SET display_name='Owner' WHERE id='00000000-0000-4000-8000-000000000002';
 GET DIAGNOSTICS n=ROW_COUNT; IF n<>1 THEN RAISE EXCEPTION 'Owner cannot update own profile'; END IF;
 UPDATE public.profiles SET display_name='Forbidden' WHERE id='00000000-0000-4000-8000-000000000003';
 GET DIAGNOSTICS n=ROW_COUNT; IF n<>0 THEN RAISE EXCEPTION 'Other profile was writable'; END IF;
 result:=public.get_records_request(current_setting('civic.test_request')::uuid);
 IF result->>'can_manage'<>'true' THEN RAISE EXCEPTION 'Owner lost managed projection'; END IF;
 PERFORM public.save_records_request(current_setting('civic.test_request')::uuid,'{"title":"Owner edit","target_organization_id":"00000000-0000-4000-8000-000000000010","visibility":"private"}');
 PERFORM public.add_records_request_item(current_setting('civic.test_request')::uuid,'link','{"title":"Evidence","url":"https://example.org/"}');
 BEGIN
  PERFORM public.save_records_request(NULL,'{"title":"Forbidden","target_organization_id":"00000000-0000-4000-8000-000000000010"}');
  RAISE EXCEPTION 'Non-admin created a request';
 EXCEPTION WHEN insufficient_privilege THEN NULL; END;
END $$;
SELECT set_config('request.jwt.claim.sub','00000000-0000-4000-8000-000000000003',true);
DO $$ DECLARE result jsonb; BEGIN
 result:=public.get_records_request(current_setting('civic.test_request')::uuid);
 IF result->>'status'<>'unavailable' OR result ? 'owner_id' OR result ? 'transfers' THEN RAISE EXCEPTION 'Private request leaked to another user'; END IF;
 BEGIN
  PERFORM public.save_records_request(current_setting('civic.test_request')::uuid,'{"title":"Forbidden"}');
  RAISE EXCEPTION 'Other user edited request';
 EXCEPTION WHEN insufficient_privilege THEN NULL; END;
END $$;
SELECT set_config('request.jwt.claim.sub','00000000-0000-4000-8000-000000000001',true);
DO $$ DECLARE n integer; BEGIN
 IF (SELECT count(*) FROM public.people)<>2 THEN RAISE EXCEPTION 'Admin cannot read private record'; END IF;
 UPDATE public.people SET full_name='Admin edit' WHERE id='00000000-0000-4000-8000-000000000011';
 GET DIAGNOSTICS n=ROW_COUNT; IF n<>1 THEN RAISE EXCEPTION 'Admin update failed'; END IF;
 INSERT INTO public.people(id,slug,full_name) VALUES('00000000-0000-4000-8000-000000000013','civic-create-test','Create test');
 DELETE FROM public.people WHERE id='00000000-0000-4000-8000-000000000013';
 GET DIAGNOSTICS n=ROW_COUNT; IF n<>1 THEN RAISE EXCEPTION 'Admin CRUD failed'; END IF;
END $$;
ROLLBACK;
