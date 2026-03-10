import type { Locale } from '../types';
import en from './en';
import zhCN from './zh-CN';
import ja from './ja';
import ko from './ko';
import vi from './vi';
import tl from './tl';
import es from './es';
import pt from './pt';
import it from './it';
import de from './de';
import fr from './fr';
import ar from './ar';
import hi from './hi';
import ru from './ru';
import bn from './bn';
import he from './he';
import pl from './pl';
import cs from './cs';
import nl from './nl';
import tr from './tr';
import uk from './uk';
import id from './id';
import th from './th';
import ur from './ur';
import ro from './ro';
import sv from './sv';
import el from './el';
import hu from './hu';
import fi from './fi';
import da from './da';
import nb from './nb';

function merge(overrides: Partial<typeof en>): Record<string, string> {
  return { ...en, ...overrides };
}

export const translations: Record<Locale, Record<string, string>> = {
  en,
  'zh-CN': merge(zhCN),
  ja: merge(ja),
  ko: merge(ko),
  vi: merge(vi),
  tl: merge(tl),
  es: merge(es),
  pt: merge(pt),
  it: merge(it),
  de: merge(de),
  fr: merge(fr),
  ar: merge(ar),
  hi: merge(hi),
  ru: merge(ru),
  bn: merge(bn),
  he: merge(he),
  pl: merge(pl),
  cs: merge(cs),
  nl: merge(nl),
  tr: merge(tr),
  uk: merge(uk),
  id: merge(id),
  th: merge(th),
  ur: merge(ur),
  ro: merge(ro),
  sv: merge(sv),
  el: merge(el),
  hu: merge(hu),
  fi: merge(fi),
  da: merge(da),
  nb: merge(nb),
};
