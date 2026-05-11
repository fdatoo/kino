import type { components } from '../../api/schema';
import { formatState } from './request-utils';

type RequestState = components['schemas']['RequestState'];

export function StatePill({ state }: { state: RequestState }) {
    return <span className="state-pill">{formatState(state)}</span>;
}
