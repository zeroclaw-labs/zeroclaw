import { createElement } from 'react';
import { NavLink, type NavLinkProps } from 'react-router-dom';

type SidebarNavLinkProps = Omit<NavLinkProps, 'end' | 'to'> & {
  activePath: string | null;
  to: string;
};

export function SidebarNavLink({
  activePath,
  to,
  ...props
}: SidebarNavLinkProps) {
  return createElement(NavLink, {
    ...props,
    to,
    end: to === '/' || to !== activePath,
  });
}
