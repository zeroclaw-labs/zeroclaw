import { createElement, type Ref } from 'react';
import { NavLink, type NavLinkProps } from 'react-router-dom';

type SidebarNavLinkProps = Omit<NavLinkProps, 'end' | 'to'> & {
  activePath: string | null;
  to: string;
  ref?: Ref<HTMLAnchorElement>;
};

export function SidebarNavLink({
  activePath,
  to,
  ref,
  ...props
}: SidebarNavLinkProps) {
  return createElement(NavLink, {
    ...props,
    ref,
    to,
    end: to === '/' || to !== activePath,
  });
}
